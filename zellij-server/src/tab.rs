//! `Tab`s holds multiple panes. It tracks their coordinates (x/y) and size,
//! as well as how they should be resized

use zellij_utils::{position::Position, serde, zellij_tile};

#[cfg(not(feature = "parametric_resize_beta"))]
use crate::ui::pane_resizer::PaneResizer;
#[cfg(feature = "parametric_resize_beta")]
use crate::ui::pane_resizer_beta::PaneResizer;
use crate::{
    os_input_output::ServerOsApi,
    panes::{PaneId, PluginPane, TerminalPane},
    pty::{PtyInstruction, VteBytes},
    thread_bus::ThreadSenders,
    ui::boundaries::Boundaries,
    wasm_vm::PluginInstruction,
    ServerInstruction, SessionState,
};
use serde::{Deserialize, Serialize};
use std::os::unix::io::RawFd;
use std::sync::{mpsc::channel, Arc, RwLock};
use std::time::Instant;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashSet},
};
use zellij_tile::data::{Event, InputMode, ModeInfo, Palette, PaletteColor};
use zellij_utils::{
    input::{
        layout::{Layout, Run},
        parse_keys,
    },
    pane_size::PositionAndSize,
};

const CURSOR_HEIGHT_WIDTH_RATIO: usize = 4; // this is not accurate and kind of a magic number, TODO: look into this

// MIN_TERMINAL_HEIGHT here must be larger than the height of any of the status bars
// this is a dirty hack until we implement fixed panes
const MIN_TERMINAL_HEIGHT: usize = 5;
const MIN_TERMINAL_WIDTH: usize = 5;

type BorderAndPaneIds = (usize, Vec<PaneId>);

fn split_vertically(rect: &PositionAndSize) -> (PositionAndSize, PositionAndSize) {
    let width_of_each_half = rect.cols / 2;
    let mut first_rect = *rect;
    let mut second_rect = *rect;
    if rect.cols % 2 == 0 {
        first_rect.cols = width_of_each_half;
    } else {
        first_rect.cols = width_of_each_half + 1;
    }
    second_rect.x = first_rect.x + first_rect.cols;
    second_rect.cols = width_of_each_half;
    (first_rect, second_rect)
}

fn split_horizontally(rect: &PositionAndSize) -> (PositionAndSize, PositionAndSize) {
    let height_of_each_half = rect.rows / 2;
    let mut first_rect = *rect;
    let mut second_rect = *rect;
    if rect.rows % 2 == 0 {
        first_rect.rows = height_of_each_half;
    } else {
        first_rect.rows = height_of_each_half + 1;
    }
    second_rect.y = first_rect.y + first_rect.rows;
    second_rect.rows = height_of_each_half;
    (first_rect, second_rect)
}

fn pane_content_offset(
    position_and_size: &PositionAndSize,
    viewport: &PositionAndSize,
) -> (usize, usize) {
    // (columns_offset, rows_offset)
    // if the pane is not on the bottom or right edge on the screen, we need to reserve one space
    // from its content to leave room for the boundary between it and the next pane (if it doesn't
    // draw its own frame)
    let columns_offset = if position_and_size.x + position_and_size.cols < viewport.cols {
        1
    } else {
        0
    };
    let rows_offset = if position_and_size.y + position_and_size.rows < viewport.rows {
        1
    } else {
        0
    };
    (columns_offset, rows_offset)
}

pub(crate) struct Tab {
    pub index: usize,
    pub position: usize,
    pub name: String,
    panes: BTreeMap<PaneId, Box<dyn Pane>>,
    panes_to_hide: HashSet<PaneId>,
    active_terminal: Option<PaneId>,
    max_panes: Option<usize>,
    viewport: PositionAndSize,     // includes all selectable panes
    display_area: PositionAndSize, // includes all panes (including eg. the status bar and tab bar in the default layout)
    fullscreen_is_active: bool,
    os_api: Box<dyn ServerOsApi>,
    pub senders: ThreadSenders,
    synchronize_is_active: bool,
    should_clear_display_before_rendering: bool,
    session_state: Arc<RwLock<SessionState>>,
    pub mode_info: ModeInfo,
    pub colors: Option<Palette>,
    draw_pane_frames: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(crate = "self::serde")]
pub(crate) struct TabData {
    /* subset of fields to publish to plugins */
    pub position: usize,
    pub name: String,
    pub active: bool,
    pub mode_info: ModeInfo,
    pub colors: Palette,
}

// FIXME: Use a struct that has a pane_type enum, to reduce all of the duplication
pub trait Pane {
    fn x(&self) -> usize;
    fn y(&self) -> usize;
    fn rows(&self) -> usize;
    fn columns(&self) -> usize;
    fn reset_size_and_position_override(&mut self);
    fn change_pos_and_size(&mut self, position_and_size: &PositionAndSize);
    fn override_size_and_position(&mut self, x: usize, y: usize, size: &PositionAndSize);
    fn handle_pty_bytes(&mut self, bytes: VteBytes);
    fn cursor_coordinates(&self) -> Option<(usize, usize)>;
    fn adjust_input_to_terminal(&self, input_bytes: Vec<u8>) -> Vec<u8>;
    fn position_and_size(&self) -> PositionAndSize;
    fn position_and_size_override(&self) -> Option<PositionAndSize>;
    fn should_render(&self) -> bool;
    fn set_should_render(&mut self, should_render: bool);
    fn set_should_render_boundaries(&mut self, _should_render: bool) {}
    fn selectable(&self) -> bool;
    fn set_selectable(&mut self, selectable: bool);
    fn set_invisible_borders(&mut self, invisible_borders: bool);
    fn set_fixed_height(&mut self, fixed_height: usize);
    fn set_fixed_width(&mut self, fixed_width: usize);
    fn render(&mut self) -> Option<String>;
    fn pid(&self) -> PaneId;
    fn reduce_height_down(&mut self, count: usize);
    fn increase_height_down(&mut self, count: usize);
    fn increase_height_up(&mut self, count: usize);
    fn reduce_height_up(&mut self, count: usize);
    fn increase_width_right(&mut self, count: usize);
    fn reduce_width_right(&mut self, count: usize);
    fn reduce_width_left(&mut self, count: usize);
    fn increase_width_left(&mut self, count: usize);
    fn push_down(&mut self, count: usize);
    fn push_right(&mut self, count: usize);
    fn pull_left(&mut self, count: usize);
    fn pull_up(&mut self, count: usize);
    fn scroll_up(&mut self, count: usize);
    fn scroll_down(&mut self, count: usize);
    fn clear_scroll(&mut self);
    fn active_at(&self) -> Instant;
    fn set_active_at(&mut self, instant: Instant);
    fn cursor_shape_csi(&self) -> String {
        "\u{1b}[0 q".to_string() // default to non blinking block
    }
    fn contains(&self, position: &Position) -> bool {
        match self.position_and_size_override() {
            Some(position_and_size) => position_and_size.contains(position),
            None => self.position_and_size().contains(position),
        }
    }
    fn start_selection(&mut self, _start: &Position) {}
    fn update_selection(&mut self, _position: &Position) {}
    fn end_selection(&mut self, _end: Option<&Position>) {}
    fn reset_selection(&mut self) {}
    fn get_selected_text(&self) -> Option<String> {
        None
    }

    fn right_boundary_x_coords(&self) -> usize {
        self.x() + self.columns()
    }
    fn bottom_boundary_y_coords(&self) -> usize {
        self.y() + self.rows()
    }
    fn is_directly_right_of(&self, other: &dyn Pane) -> bool {
        self.x() == other.x() + other.columns()
    }
    fn is_directly_left_of(&self, other: &dyn Pane) -> bool {
        self.x() + self.columns() == other.x()
    }
    fn is_directly_below(&self, other: &dyn Pane) -> bool {
        self.y() == other.y() + other.rows()
    }
    fn is_directly_above(&self, other: &dyn Pane) -> bool {
        self.y() + self.rows() == other.y()
    }
    fn horizontally_overlaps_with(&self, other: &dyn Pane) -> bool {
        (self.y() >= other.y() && self.y() < (other.y() + other.rows()))
            || ((self.y() + self.rows()) <= (other.y() + other.rows())
                && (self.y() + self.rows()) > other.y())
            || (self.y() <= other.y() && (self.y() + self.rows() >= (other.y() + other.rows())))
            || (other.y() <= self.y() && (other.y() + other.rows() >= (self.y() + self.rows())))
    }
    fn get_horizontal_overlap_with(&self, other: &dyn Pane) -> usize {
        std::cmp::min(self.y() + self.rows(), other.y() + other.rows())
            - std::cmp::max(self.y(), other.y())
    }
    fn vertically_overlaps_with(&self, other: &dyn Pane) -> bool {
        (self.x() >= other.x() && self.x() < (other.x() + other.columns()))
            || ((self.x() + self.columns()) <= (other.x() + other.columns())
                && (self.x() + self.columns()) > other.x())
            || (self.x() <= other.x()
                && (self.x() + self.columns() >= (other.x() + other.columns())))
            || (other.x() <= self.x()
                && (other.x() + other.columns() >= (self.x() + self.columns())))
    }
    fn get_vertical_overlap_with(&self, other: &dyn Pane) -> usize {
        std::cmp::min(self.x() + self.columns(), other.x() + other.columns())
            - std::cmp::max(self.x(), other.x())
    }
    fn can_increase_height_by(&self, increase_by: usize) -> bool {
        self.max_height()
            .map(|max_height| self.rows() + increase_by <= max_height)
            .unwrap_or(true)
    }
    fn can_increase_width_by(&self, increase_by: usize) -> bool {
        self.max_width()
            .map(|max_width| self.columns() + increase_by <= max_width)
            .unwrap_or(true)
    }
    fn can_reduce_height_by(&self, reduce_by: usize) -> bool {
        self.rows() > reduce_by && self.rows() - reduce_by >= self.min_height()
    }
    fn can_reduce_width_by(&self, reduce_by: usize) -> bool {
        self.columns() > reduce_by && self.columns() - reduce_by >= self.min_width()
    }
    fn min_width(&self) -> usize {
        MIN_TERMINAL_WIDTH
    }
    fn min_height(&self) -> usize {
        MIN_TERMINAL_HEIGHT
    }
    fn max_width(&self) -> Option<usize> {
        None
    }
    fn max_height(&self) -> Option<usize> {
        None
    }
    fn invisible_borders(&self) -> bool {
        false
    }
    fn drain_messages_to_pty(&mut self) -> Vec<Vec<u8>> {
        // TODO: this is only relevant to terminal panes
        // we should probably refactor away from this trait at some point
        vec![]
    }
    fn render_full_viewport(&mut self) {}
    fn relative_position(&self, position: &Position) -> Position {
        match self.position_and_size_override() {
            Some(position_and_size) => position.relative_to(&position_and_size),
            None => position.relative_to(&self.position_and_size()),
        }
    }
    fn get_content_rows(&self) -> usize {
        // content rows might differ from the pane's rows if the pane has a frame
        // in that case they would be 2 less
        self.rows()
    }
    fn get_content_columns(&self) -> usize {
        // content columns might differ from the pane's columns if the pane has a frame
        // in that case they would be 2 less
        self.columns()
    }
    fn set_boundary_color(&mut self, _color: Option<PaletteColor>) {}
    fn offset_content_columns(&mut self, _by: usize) {}
    fn offset_content_rows(&mut self, _by: usize) {}
    fn show_boundaries_frame(&mut self, _render_only_title: bool) {}
    fn remove_boundaries_frame(&mut self) {}
}

impl Tab {
    // FIXME: Still too many arguments for clippy to be happy...
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        index: usize,
        position: usize,
        name: String,
        viewport: &PositionAndSize,
        os_api: Box<dyn ServerOsApi>,
        senders: ThreadSenders,
        max_panes: Option<usize>,
        pane_id: Option<PaneId>,
        mode_info: ModeInfo,
        colors: Option<Palette>,
        session_state: Arc<RwLock<SessionState>>,
        draw_pane_frames: bool,
    ) -> Self {
        let panes = if let Some(PaneId::Terminal(pid)) = pane_id {
            let pane_title_only = true;
            let mut new_terminal = TerminalPane::new(pid, *viewport, colors, 1);
            if draw_pane_frames {
                new_terminal.show_boundaries_frame(pane_title_only);
            }
            os_api.set_terminal_size_using_fd(
                new_terminal.pid,
                new_terminal.columns() as u16,
                new_terminal.rows() as u16,
            );
            let mut panes: BTreeMap<PaneId, Box<dyn Pane>> = BTreeMap::new();
            panes.insert(PaneId::Terminal(pid), Box::new(new_terminal));
            panes
        } else {
            BTreeMap::new()
        };

        let name = if name.is_empty() {
            format!("Tab #{}", position + 1)
        } else {
            name
        };

        Tab {
            index,
            position,
            panes,
            name,
            max_panes,
            panes_to_hide: HashSet::new(),
            active_terminal: pane_id,
            viewport: *viewport,
            display_area: *viewport,
            fullscreen_is_active: false,
            synchronize_is_active: false,
            os_api,
            senders,
            should_clear_display_before_rendering: false,
            mode_info,
            colors,
            session_state,
            draw_pane_frames,
        }
    }

    pub fn apply_layout(&mut self, layout: Layout, new_pids: Vec<RawFd>, tab_index: usize) {
        // TODO: this should be an attribute on Screen instead of viewport
        let free_space = PositionAndSize {
            x: 0,
            y: 0,
            rows: self.viewport.rows,
            cols: self.viewport.cols,
            ..Default::default()
        };
        self.panes_to_hide.clear();
        let positions_in_layout = layout.position_panes_in_space(&free_space);

        for (layout, position_and_size) in &positions_in_layout {
            // we need to do this first because it decides the size of the screen
            // which we use for other stuff in the main loop below (eg. which type of frames the
            // panes should have)
            if layout.borderless {
                self.offset_viewport(position_and_size);
            }
        }

        let mut positions_and_size = positions_in_layout.iter();
        let total_borderless_panes = layout.total_borderless_panes();
        let total_panes_with_border = positions_in_layout
            .len()
            .saturating_sub(total_borderless_panes);
        for (pane_kind, terminal_pane) in self.panes.iter_mut() {
            // for now the layout only supports terminal panes
            if let PaneId::Terminal(pid) = pane_kind {
                match positions_and_size.next() {
                    Some((_, position_and_size)) => {
                        terminal_pane.reset_size_and_position_override();
                        terminal_pane.change_pos_and_size(position_and_size);
                        self.os_api.set_terminal_size_using_fd(
                            *pid,
                            position_and_size.cols as u16,
                            position_and_size.rows as u16,
                        );
                    }
                    None => {
                        // we filled the entire layout, no room for this pane
                        // TODO: handle active terminal
                        self.panes_to_hide.insert(PaneId::Terminal(*pid));
                    }
                }
            }
        }
        let mut new_pids = new_pids.iter();

        for (layout, position_and_size) in positions_and_size {
            // A plugin pane
            if let Some(Run::Plugin(Some(plugin))) = &layout.run {
                let (pid_tx, pid_rx) = channel();
                self.senders
                    .send_to_plugin(PluginInstruction::Load(pid_tx, plugin.clone(), tab_index))
                    .unwrap();
                let pid = pid_rx.recv().unwrap();
                let draw_pane_frames = self.draw_pane_frames && !layout.borderless;
                let pane_title_only = !layout.borderless && total_panes_with_border == 1;
                let title = String::from(plugin.as_path().as_os_str().to_string_lossy());
                let mut new_plugin = PluginPane::new(
                    pid,
                    *position_and_size,
                    self.senders.to_plugin.as_ref().unwrap().clone(),
                    title,
                );
                if draw_pane_frames && !layout.borderless {
                    new_plugin.show_boundaries_frame(pane_title_only);
                }
                self.panes.insert(PaneId::Plugin(pid), Box::new(new_plugin));
                // Send an initial mode update to the newly loaded plugin only!
                self.senders
                    .send_to_plugin(PluginInstruction::Update(
                        Some(pid),
                        Event::ModeUpdate(self.mode_info.clone()),
                    ))
                    .unwrap();
            } else {
                // there are still panes left to fill, use the pids we received in this method
                let pid = new_pids.next().unwrap(); // if this crashes it means we got less pids than there are panes in this layout
                let next_selectable_pane_position = self.get_next_selectable_pane_position();
                let pane_title_only =
                    next_selectable_pane_position == 1 && total_panes_with_border == 1;
                let draw_pane_frames = self.draw_pane_frames && !layout.borderless;
                let mut new_terminal = TerminalPane::new(
                    *pid,
                    *position_and_size,
                    self.colors,
                    next_selectable_pane_position,
                );
                if draw_pane_frames {
                    new_terminal.show_boundaries_frame(pane_title_only);
                } else {
                    let (pane_columns_offset, pane_rows_offset) =
                        pane_content_offset(position_and_size, &self.viewport);
                    new_terminal.offset_content_columns(pane_columns_offset);
                    new_terminal.offset_content_rows(pane_rows_offset);
                }
                self.os_api.set_terminal_size_using_fd(
                    new_terminal.pid,
                    new_terminal.get_content_columns() as u16,
                    new_terminal.get_content_rows() as u16,
                );
                self.panes
                    .insert(PaneId::Terminal(*pid), Box::new(new_terminal));
            }
        }
        for unused_pid in new_pids {
            // this is a bit of a hack and happens because we don't have any central location that
            // can query the screen as to how many panes it needs to create a layout
            // fixing this will require a bit of an architecture change
            self.senders
                .send_to_pty(PtyInstruction::ClosePane(PaneId::Terminal(*unused_pid)))
                .unwrap();
        }
        self.active_terminal = self.panes.iter().map(|(id, _)| id.to_owned()).next();
        self.render();
    }
    pub fn new_pane(&mut self, pid: PaneId) {
        self.close_down_to_max_terminals();
        if self.fullscreen_is_active {
            self.toggle_active_pane_fullscreen();
        }
        if !self.has_panes() {
            if let PaneId::Terminal(term_pid) = pid {
                let next_selectable_pane_position = self.get_next_selectable_pane_position();
                let pane_title_only = next_selectable_pane_position == 1;
                let mut new_terminal = TerminalPane::new(
                    term_pid,
                    self.viewport,
                    self.colors,
                    next_selectable_pane_position,
                );
                if self.draw_pane_frames {
                    new_terminal.show_boundaries_frame(pane_title_only);
                }
                self.os_api.set_terminal_size_using_fd(
                    new_terminal.pid,
                    new_terminal.columns() as u16,
                    new_terminal.rows() as u16,
                );
                self.panes.insert(pid, Box::new(new_terminal));
                self.active_terminal = Some(pid);
            }
        } else {
            // TODO: check minimum size of active terminal

            let (_largest_terminal_size, terminal_id_to_split) = self.get_panes().fold(
                (0, None),
                |(current_largest_terminal_size, current_terminal_id_to_split),
                 id_and_terminal_to_check| {
                    let (id_of_terminal_to_check, terminal_to_check) = id_and_terminal_to_check;
                    let terminal_size = (terminal_to_check.rows() * CURSOR_HEIGHT_WIDTH_RATIO)
                        * terminal_to_check.columns();
                    let terminal_can_be_split = terminal_to_check.columns() >= MIN_TERMINAL_WIDTH
                        && terminal_to_check.rows() >= MIN_TERMINAL_HEIGHT
                        && ((terminal_to_check.columns() > terminal_to_check.min_width() * 2)
                            || (terminal_to_check.rows() > terminal_to_check.min_height() * 2));
                    if terminal_can_be_split && terminal_size > current_largest_terminal_size {
                        (terminal_size, Some(*id_of_terminal_to_check))
                    } else {
                        (current_largest_terminal_size, current_terminal_id_to_split)
                    }
                },
            );
            if terminal_id_to_split.is_none() {
                self.senders
                    .send_to_pty(PtyInstruction::ClosePane(pid)) // we can't open this pane, close the pty
                    .unwrap();
                return; // likely no terminal large enough to split
            }
            let terminal_id_to_split = terminal_id_to_split.unwrap();
            let next_selectable_pane_position = self.get_next_selectable_pane_position();
            let terminal_to_split = self.panes.get_mut(&terminal_id_to_split).unwrap();
            let terminal_ws = PositionAndSize {
                rows: terminal_to_split.rows(),
                cols: terminal_to_split.columns(),
                x: terminal_to_split.x(),
                y: terminal_to_split.y(),
                ..Default::default()
            };
            if terminal_to_split.rows() * CURSOR_HEIGHT_WIDTH_RATIO > terminal_to_split.columns()
                && terminal_to_split.rows() > terminal_to_split.min_height() * 2
            {
                if let PaneId::Terminal(term_pid) = pid {
                    let (top_winsize, bottom_winsize) = split_horizontally(&terminal_ws);
                    let pane_title_only = next_selectable_pane_position == 1;
                    let mut new_terminal = TerminalPane::new(
                        term_pid,
                        bottom_winsize,
                        self.colors,
                        next_selectable_pane_position,
                    );
                    if self.draw_pane_frames {
                        new_terminal.show_boundaries_frame(pane_title_only);
                    } else {
                        let (pane_columns_offset, pane_rows_offset) =
                            pane_content_offset(&bottom_winsize, &self.viewport);
                        new_terminal.offset_content_columns(pane_columns_offset);
                        new_terminal.offset_content_rows(pane_rows_offset);
                    }
                    self.os_api.set_terminal_size_using_fd(
                        new_terminal.pid,
                        new_terminal.get_content_columns() as u16,
                        new_terminal.get_content_rows() as u16,
                    );
                    if self.draw_pane_frames {
                        let only_title = false;
                        terminal_to_split.show_boundaries_frame(only_title);
                    } else {
                        let (pane_columns_offset, pane_rows_offset) =
                            pane_content_offset(&top_winsize, &self.viewport);
                        terminal_to_split.offset_content_columns(pane_columns_offset);
                        terminal_to_split.offset_content_rows(pane_rows_offset);
                    }
                    terminal_to_split.change_pos_and_size(&top_winsize);
                    let terminal_to_split_content_columns = terminal_to_split.get_content_columns();
                    let terminal_to_split_content_rows = terminal_to_split.get_content_rows();
                    self.panes.insert(pid, Box::new(new_terminal));
                    if let PaneId::Terminal(terminal_id_to_split) = terminal_id_to_split {
                        self.os_api.set_terminal_size_using_fd(
                            terminal_id_to_split,
                            terminal_to_split_content_columns as u16,
                            terminal_to_split_content_rows as u16,
                        );
                    }
                    self.active_terminal = Some(pid);
                }
            } else if terminal_to_split.columns() > terminal_to_split.min_width() * 2 {
                if let PaneId::Terminal(term_pid) = pid {
                    let (left_winsize, right_winsize) = split_vertically(&terminal_ws);
                    let pane_title_only = next_selectable_pane_position == 1;
                    let mut new_terminal = TerminalPane::new(
                        term_pid,
                        right_winsize,
                        self.colors,
                        next_selectable_pane_position,
                    );
                    if self.draw_pane_frames {
                        new_terminal.show_boundaries_frame(pane_title_only);
                    } else {
                        let (pane_columns_offset, pane_rows_offset) =
                            pane_content_offset(&right_winsize, &self.viewport);
                        new_terminal.offset_content_columns(pane_columns_offset);
                        new_terminal.offset_content_rows(pane_rows_offset);
                    }
                    self.os_api.set_terminal_size_using_fd(
                        new_terminal.pid,
                        new_terminal.get_content_columns() as u16,
                        new_terminal.get_content_rows() as u16,
                    );
                    if self.draw_pane_frames {
                        let only_title = false;
                        terminal_to_split.show_boundaries_frame(only_title);
                    } else {
                        let (pane_columns_offset, pane_rows_offset) =
                            pane_content_offset(&left_winsize, &self.viewport);
                        terminal_to_split.offset_content_columns(pane_columns_offset);
                        terminal_to_split.offset_content_rows(pane_rows_offset);
                    }
                    terminal_to_split.change_pos_and_size(&left_winsize);
                    let terminal_to_split_content_columns = terminal_to_split.get_content_columns();
                    let terminal_to_split_content_rows = terminal_to_split.get_content_rows();
                    self.panes.insert(pid, Box::new(new_terminal));
                    if let PaneId::Terminal(terminal_id_to_split) = terminal_id_to_split {
                        self.os_api.set_terminal_size_using_fd(
                            terminal_id_to_split,
                            terminal_to_split_content_columns as u16,
                            terminal_to_split_content_rows as u16,
                        );
                    }
                }
            }
            self.active_terminal = Some(pid);
            self.render();
        }
    }
    pub fn horizontal_split(&mut self, pid: PaneId) {
        self.close_down_to_max_terminals();
        if self.fullscreen_is_active {
            self.toggle_active_pane_fullscreen();
        }
        if !self.has_panes() {
            if let PaneId::Terminal(term_pid) = pid {
                let next_selectable_pane_position = self.get_next_selectable_pane_position();
                let pane_title_only = next_selectable_pane_position == 1;
                let mut new_terminal = TerminalPane::new(
                    term_pid,
                    self.viewport,
                    self.colors,
                    next_selectable_pane_position,
                );
                if self.draw_pane_frames {
                    new_terminal.show_boundaries_frame(pane_title_only);
                }
                self.os_api.set_terminal_size_using_fd(
                    new_terminal.pid,
                    new_terminal.get_content_columns() as u16,
                    new_terminal.get_content_rows() as u16,
                );
                self.panes.insert(pid, Box::new(new_terminal));
                self.active_terminal = Some(pid);
            }
        } else if let PaneId::Terminal(term_pid) = pid {
            let active_pane_id = &self.get_active_pane_id().unwrap();
            let active_pane = self.panes.get_mut(active_pane_id).unwrap();
            if active_pane.rows() < MIN_TERMINAL_HEIGHT * 2 {
                self.senders
                    .send_to_pty(PtyInstruction::ClosePane(pid)) // we can't open this pane, close the pty
                    .unwrap();
                return;
            }
            let terminal_ws = PositionAndSize {
                x: active_pane.x(),
                y: active_pane.y(),
                rows: active_pane.rows(),
                cols: active_pane.columns(),
                ..Default::default()
            };
            let (top_winsize, bottom_winsize) = split_horizontally(&terminal_ws);

            if self.draw_pane_frames {
                let only_title = false;
                active_pane.show_boundaries_frame(only_title);
            } else {
                let (pane_columns_offset, pane_rows_offset) =
                    pane_content_offset(&top_winsize, &self.viewport);
                active_pane.offset_content_columns(pane_columns_offset);
                active_pane.offset_content_rows(pane_rows_offset);
            }
            active_pane.change_pos_and_size(&top_winsize);

            let active_pane_content_columns = active_pane.get_content_columns();
            let active_pane_content_rows = active_pane.get_content_rows();

            let next_selectable_pane_position = self.get_next_selectable_pane_position();
            let pane_title_only = next_selectable_pane_position == 1;
            let mut new_terminal = TerminalPane::new(
                term_pid,
                bottom_winsize,
                self.colors,
                next_selectable_pane_position,
            );
            if self.draw_pane_frames {
                new_terminal.show_boundaries_frame(pane_title_only);
            } else {
                let (pane_columns_offset, pane_rows_offset) =
                    pane_content_offset(&bottom_winsize, &self.viewport);
                new_terminal.offset_content_columns(pane_columns_offset);
                new_terminal.offset_content_rows(pane_rows_offset);
            }
            self.os_api.set_terminal_size_using_fd(
                new_terminal.pid,
                new_terminal.get_content_columns() as u16,
                new_terminal.get_content_rows() as u16,
            );
            self.panes.insert(pid, Box::new(new_terminal));

            if let PaneId::Terminal(active_terminal_pid) = active_pane_id {
                self.os_api.set_terminal_size_using_fd(
                    *active_terminal_pid,
                    active_pane_content_columns as u16,
                    active_pane_content_rows as u16,
                );
            }

            self.active_terminal = Some(pid);
            self.render();
        }
    }
    pub fn vertical_split(&mut self, pid: PaneId) {
        self.close_down_to_max_terminals();
        if self.fullscreen_is_active {
            self.toggle_active_pane_fullscreen();
        }
        if !self.has_panes() {
            if let PaneId::Terminal(term_pid) = pid {
                let next_selectable_pane_position = self.get_next_selectable_pane_position();
                let pane_title_only = next_selectable_pane_position == 1;
                let mut new_terminal = TerminalPane::new(
                    term_pid,
                    self.viewport,
                    self.colors,
                    next_selectable_pane_position,
                );
                if self.draw_pane_frames {
                    new_terminal.show_boundaries_frame(pane_title_only);
                }
                self.os_api.set_terminal_size_using_fd(
                    new_terminal.pid,
                    new_terminal.get_content_columns() as u16,
                    new_terminal.get_content_rows() as u16,
                );
                self.panes.insert(pid, Box::new(new_terminal));
                self.active_terminal = Some(pid);
            }
        } else if let PaneId::Terminal(term_pid) = pid {
            // TODO: check minimum size of active terminal
            let active_pane_id = &self.get_active_pane_id().unwrap();
            let active_pane = self.panes.get_mut(active_pane_id).unwrap();
            if active_pane.columns() < MIN_TERMINAL_WIDTH * 2 {
                self.senders
                    .send_to_pty(PtyInstruction::ClosePane(pid)) // we can't open this pane, close the pty
                    .unwrap();
                return;
            }
            let terminal_ws = PositionAndSize {
                x: active_pane.x(),
                y: active_pane.y(),
                rows: active_pane.rows(),
                cols: active_pane.columns(),
                ..Default::default()
            };
            let (left_winsize, right_winsize) = split_vertically(&terminal_ws);

            if self.draw_pane_frames {
                let only_title = false;
                active_pane.show_boundaries_frame(only_title);
            } else {
                let (pane_columns_offset, pane_rows_offset) =
                    pane_content_offset(&left_winsize, &self.viewport);
                active_pane.offset_content_columns(pane_columns_offset);
                active_pane.offset_content_rows(pane_rows_offset);
            }
            active_pane.change_pos_and_size(&left_winsize);

            let active_pane_content_columns = active_pane.get_content_columns();
            let active_pane_content_rows = active_pane.get_content_rows();

            let next_selectable_pane_position = self.get_next_selectable_pane_position();
            let pane_title_only = next_selectable_pane_position == 1;
            let mut new_terminal = TerminalPane::new(
                term_pid,
                right_winsize,
                self.colors,
                next_selectable_pane_position,
            );
            if self.draw_pane_frames {
                new_terminal.show_boundaries_frame(pane_title_only);
            } else {
                let (pane_columns_offset, pane_rows_offset) =
                    pane_content_offset(&right_winsize, &self.viewport);
                new_terminal.offset_content_columns(pane_columns_offset);
                new_terminal.offset_content_rows(pane_rows_offset);
            }
            self.os_api.set_terminal_size_using_fd(
                new_terminal.pid,
                new_terminal.get_content_columns() as u16,
                new_terminal.get_content_rows() as u16,
            );
            self.panes.insert(pid, Box::new(new_terminal));

            if let PaneId::Terminal(active_terminal_pid) = active_pane_id {
                self.os_api.set_terminal_size_using_fd(
                    *active_terminal_pid,
                    active_pane_content_columns as u16,
                    active_pane_content_rows as u16,
                );
            }

            self.active_terminal = Some(pid);
            self.render();
        }
    }
    pub fn get_active_pane(&self) -> Option<&dyn Pane> {
        // FIXME: Could use Option::map() here
        match self.get_active_pane_id() {
            Some(active_pane) => self.panes.get(&active_pane).map(Box::as_ref),
            None => None,
        }
    }
    fn get_active_pane_id(&self) -> Option<PaneId> {
        self.active_terminal
    }
    fn get_active_terminal_id(&self) -> Option<RawFd> {
        // FIXME: Is there a better way to do this?
        if let Some(PaneId::Terminal(pid)) = self.active_terminal {
            Some(pid)
        } else {
            None
        }
    }
    pub fn has_terminal_pid(&self, pid: RawFd) -> bool {
        self.panes.contains_key(&PaneId::Terminal(pid))
    }
    pub fn handle_pty_bytes(&mut self, pid: RawFd, bytes: VteBytes) {
        // if we don't have the terminal in self.terminals it's probably because
        // of a race condition where the terminal was created in pty but has not
        // yet been created in Screen. These events are currently not buffered, so
        // if you're debugging seemingly randomly missing stdout data, this is
        // the reason
        if let Some(terminal_output) = self.panes.get_mut(&PaneId::Terminal(pid)) {
            terminal_output.handle_pty_bytes(bytes);
            let messages_to_pty = terminal_output.drain_messages_to_pty();
            for message in messages_to_pty {
                self.write_to_pane_id(message, PaneId::Terminal(pid));
            }
            // self.render();
        }
    }
    pub fn write_to_terminals_on_current_tab(&mut self, input_bytes: Vec<u8>) {
        let pane_ids = self.get_pane_ids();
        pane_ids.iter().for_each(|&pane_id| {
            self.write_to_pane_id(input_bytes.clone(), pane_id);
        });
    }
    pub fn write_to_active_terminal(&mut self, input_bytes: Vec<u8>) {
        self.write_to_pane_id(input_bytes, self.get_active_pane_id().unwrap());
    }
    pub fn write_to_pane_id(&mut self, input_bytes: Vec<u8>, pane_id: PaneId) {
        match pane_id {
            PaneId::Terminal(active_terminal_id) => {
                let active_terminal = self.panes.get(&pane_id).unwrap();
                let adjusted_input = active_terminal.adjust_input_to_terminal(input_bytes);
                self.os_api
                    .write_to_tty_stdin(active_terminal_id, &adjusted_input)
                    .expect("failed to write to terminal");
                self.os_api
                    .tcdrain(active_terminal_id)
                    .expect("failed to drain terminal");
            }
            PaneId::Plugin(pid) => {
                for key in parse_keys(&input_bytes) {
                    self.senders
                        .send_to_plugin(PluginInstruction::Update(Some(pid), Event::KeyPress(key)))
                        .unwrap()
                }
            }
        }
    }
    pub fn get_active_terminal_cursor_position(&self) -> Option<(usize, usize)> {
        // (x, y)
        let active_terminal = &self.get_active_pane()?;
        active_terminal
            .cursor_coordinates()
            .map(|(x_in_terminal, y_in_terminal)| {
                let x = active_terminal.x() + x_in_terminal;
                let y = active_terminal.y() + y_in_terminal;
                (x, y)
            })
    }
    pub fn toggle_active_pane_fullscreen(&mut self) {
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if self.fullscreen_is_active {
                for terminal_id in self.panes_to_hide.iter() {
                    let pane = self.panes.get_mut(terminal_id).unwrap();
                    pane.set_should_render(true);
                    pane.set_should_render_boundaries(true);
                }
                self.panes_to_hide.clear();
                let selectable_pane_count = self.get_selectable_pane_count();
                let active_terminal = self.panes.get_mut(&active_pane_id).unwrap();
                if selectable_pane_count > 1 && self.draw_pane_frames {
                    let only_title = false;
                    active_terminal.show_boundaries_frame(only_title);
                }
                if !self.draw_pane_frames {
                    let (pane_columns_offset, pane_rows_offset) =
                        pane_content_offset(&active_terminal.position_and_size(), &self.viewport);
                    active_terminal.offset_content_columns(pane_columns_offset);
                    active_terminal.offset_content_rows(pane_rows_offset);
                }
                active_terminal.reset_size_and_position_override();
            } else {
                let panes = self.get_panes();
                let pane_ids_to_hide = panes.filter_map(|(&id, _pane)| {
                    if id != active_pane_id && self.is_inside_viewport(&id) {
                        Some(id)
                    } else {
                        None
                    }
                });
                self.panes_to_hide = pane_ids_to_hide.collect();
                if self.panes_to_hide.is_empty() {
                    // nothing to do, pane is already as fullscreen as it can be, let's bail
                    return;
                } else {
                    let active_terminal = self.panes.get_mut(&active_pane_id).unwrap();
                    if self.draw_pane_frames {
                        // full screen panes don't need their full frame
                        let only_title = true;
                        active_terminal.show_boundaries_frame(only_title);
                    } else {
                        active_terminal.offset_content_rows(0);
                        active_terminal.offset_content_columns(0);
                    }
                    active_terminal.override_size_and_position(
                        self.viewport.x,
                        self.viewport.y,
                        &self.viewport,
                    );
                }
            }
            let active_terminal = self.panes.get(&active_pane_id).unwrap();
            if let PaneId::Terminal(active_pid) = active_pane_id {
                self.os_api.set_terminal_size_using_fd(
                    active_pid,
                    active_terminal.get_content_columns() as u16,
                    active_terminal.get_content_rows() as u16,
                );
            }
            self.set_force_render();
            self.render();
            self.toggle_fullscreen_is_active();
        }
    }
    pub fn toggle_fullscreen_is_active(&mut self) {
        self.fullscreen_is_active = !self.fullscreen_is_active;
    }
    pub fn set_force_render(&mut self) {
        for pane in self.panes.values_mut() {
            pane.set_should_render(true);
            pane.set_should_render_boundaries(true);
            pane.render_full_viewport();
        }
    }
    pub fn is_sync_panes_active(&self) -> bool {
        self.synchronize_is_active
    }
    pub fn toggle_sync_panes_is_active(&mut self) {
        self.synchronize_is_active = !self.synchronize_is_active;
    }
    pub fn mark_active_pane_for_rerender(&mut self) {
        if let Some(active_terminal) = self
            .active_terminal
            .and_then(|active_terminal_id| self.panes.get_mut(&active_terminal_id))
        {
            active_terminal.set_should_render(true)
        }
        //             .and_then(|active_terminal_id| self.panes.get_mut(&active_terminal_id)) {
        //                 active_terminal.set_should_render(true)
        //             }
    }
    pub fn set_pane_frames(&mut self, draw_pane_frames: bool) {
        self.draw_pane_frames = draw_pane_frames;
        let selectable_pane_count = self.panes.iter().filter(|(_, p)| p.selectable()).count();
        for (pane_id, pane) in self.panes.iter_mut() {
            if draw_pane_frames {
                let should_render_only_title = (selectable_pane_count == 1
                    && self.active_terminal == Some(*pane_id))
                    || (self.fullscreen_is_active && self.active_terminal == Some(*pane_id));
                pane.offset_content_rows(0);
                pane.offset_content_columns(0);
                pane.show_boundaries_frame(should_render_only_title);
            } else {
                let position_and_size = pane
                    .position_and_size_override()
                    .unwrap_or_else(|| pane.position_and_size());
                pane.remove_boundaries_frame();

                let (pane_columns_offset, pane_rows_offset) =
                    pane_content_offset(&position_and_size, &self.viewport);
                pane.offset_content_columns(pane_columns_offset);
                pane.offset_content_rows(pane_rows_offset);
            }
            if let PaneId::Terminal(pid) = pane_id {
                self.os_api.set_terminal_size_using_fd(
                    *pid,
                    pane.get_content_columns() as u16,
                    pane.get_content_rows() as u16,
                );
            }
        }
    }
    pub fn render(&mut self) {
        if self.active_terminal.is_none()
            || *self.session_state.read().unwrap() != SessionState::Attached
        {
            // we might not have an active terminal if we closed the last pane
            // in that case, we should not render as the app is exiting
            // or if this session is not attached to a client, we do not have to render
            return;
        }
        let mut output = String::new();
        let mut boundaries = Boundaries::new(&self.viewport);
        let hide_cursor = "\u{1b}[?25l";
        output.push_str(hide_cursor);
        if self.should_clear_display_before_rendering {
            let clear_display = "\u{1b}[2J";
            output.push_str(clear_display);
            self.should_clear_display_before_rendering = false;
        }
        for (_kind, pane) in self.panes.iter_mut() {
            if !self.panes_to_hide.contains(&pane.pid()) {
                match self.active_terminal.unwrap() == pane.pid() {
                    true => {
                        pane.set_active_at(Instant::now());
                        match self.mode_info.mode {
                            InputMode::Normal | InputMode::Locked => {
                                pane.set_boundary_color(self.colors.map(|colors| colors.green));
                            }
                            _ => {
                                pane.set_boundary_color(self.colors.map(|colors| colors.orange));
                            }
                        }
                        if !self.draw_pane_frames {
                            boundaries.add_rect(pane.as_ref(), self.mode_info.mode, self.colors)
                        }
                    }
                    false => {
                        pane.set_boundary_color(None);
                        if !pane.invisible_borders() && !self.draw_pane_frames {
                            boundaries.add_rect(pane.as_ref(), self.mode_info.mode, None);
                        }
                    }
                }
                if let Some(vte_output) = pane.render() {
                    // FIXME: Use Termion for cursor and style clearing?
                    output.push_str(&format!(
                        "\u{1b}[{};{}H\u{1b}[m{}",
                        pane.y() + 1,
                        pane.x() + 1,
                        vte_output
                    ));
                }
            }
        }

        if !self.draw_pane_frames {
            output.push_str(&boundaries.vte_output());
        }

        match self.get_active_terminal_cursor_position() {
            Some((cursor_position_x, cursor_position_y)) => {
                let show_cursor = "\u{1b}[?25h";
                let change_cursor_shape = self.get_active_pane().unwrap().cursor_shape_csi();
                let goto_cursor_position = &format!(
                    "\u{1b}[{};{}H\u{1b}[m{}",
                    cursor_position_y + 1,
                    cursor_position_x + 1,
                    change_cursor_shape
                ); // goto row/col
                output.push_str(show_cursor);
                output.push_str(goto_cursor_position);
            }
            None => {
                let hide_cursor = "\u{1b}[?25l";
                output.push_str(hide_cursor);
            }
        }

        self.senders
            .send_to_server(ServerInstruction::Render(Some(output)))
            .unwrap();
    }
    fn get_panes(&self) -> impl Iterator<Item = (&PaneId, &Box<dyn Pane>)> {
        self.panes.iter()
    }
    // FIXME: This is some shameful duplication...
    fn get_selectable_panes(&self) -> impl Iterator<Item = (&PaneId, &Box<dyn Pane>)> {
        self.panes.iter().filter(|(_, p)| p.selectable())
    }
    fn get_selectable_pane_count(&self) -> usize {
        self.get_selectable_panes().count()
    }
    fn get_next_selectable_pane_position(&self) -> usize {
        self.panes
            .iter()
            .filter(|(k, _)| match k {
                PaneId::Plugin(_) => false,
                PaneId::Terminal(_) => true,
            })
            .count()
            + 1
    }
    fn is_the_only_selectable_pane(&self, pane_id: &PaneId) -> bool {
        let selectable_panes = self.get_selectable_panes();
        if selectable_panes.count() == 1 {
            let pane = self.panes.get(pane_id);
            pane.map(|pane| pane.selectable()).unwrap_or(false)
        } else {
            false
        }
    }
    fn has_panes(&self) -> bool {
        let mut all_terminals = self.get_panes();
        all_terminals.next().is_some()
    }
    fn has_selectable_panes(&self) -> bool {
        let mut all_terminals = self.get_selectable_panes();
        all_terminals.next().is_some()
    }
    fn next_active_pane(&self, panes: &[PaneId]) -> Option<PaneId> {
        panes
            .iter()
            .rev()
            .find(|pid| self.panes.get(pid).unwrap().selectable())
            .copied()
    }
    fn pane_ids_directly_left_of(&self, id: &PaneId) -> Option<Vec<PaneId>> {
        let mut ids = vec![];
        let terminal_to_check = self.panes.get(id).unwrap();
        if terminal_to_check.x() == 0 {
            return None;
        }
        for (&pid, terminal) in self.get_panes() {
            if terminal.x() + terminal.columns() == terminal_to_check.x() {
                ids.push(pid);
            }
        }
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }
    fn pane_ids_directly_right_of(&self, id: &PaneId) -> Option<Vec<PaneId>> {
        let mut ids = vec![];
        let terminal_to_check = self.panes.get(id).unwrap();
        for (&pid, terminal) in self.get_panes() {
            if terminal.x() == terminal_to_check.x() + terminal_to_check.columns() {
                ids.push(pid);
            }
        }
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }
    fn pane_ids_directly_below(&self, id: &PaneId) -> Option<Vec<PaneId>> {
        let mut ids = vec![];
        let terminal_to_check = self.panes.get(id).unwrap();
        for (&pid, terminal) in self.get_panes() {
            if terminal.y() == terminal_to_check.y() + terminal_to_check.rows() {
                ids.push(pid);
            }
        }
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }
    fn pane_ids_directly_above(&self, id: &PaneId) -> Option<Vec<PaneId>> {
        let mut ids = vec![];
        let terminal_to_check = self.panes.get(id).unwrap();
        for (&pid, terminal) in self.get_panes() {
            if terminal.y() + terminal.rows() == terminal_to_check.y() {
                ids.push(pid);
            }
        }
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }
    fn panes_top_aligned_with_pane(&self, pane: &dyn Pane) -> Vec<&dyn Pane> {
        self.panes
            .keys()
            .map(|t_id| self.panes.get(t_id).unwrap().as_ref())
            .filter(|terminal| terminal.pid() != pane.pid() && terminal.y() == pane.y())
            .collect()
    }
    fn panes_bottom_aligned_with_pane(&self, pane: &dyn Pane) -> Vec<&dyn Pane> {
        self.panes
            .keys()
            .map(|t_id| self.panes.get(t_id).unwrap().as_ref())
            .filter(|terminal| {
                terminal.pid() != pane.pid()
                    && terminal.y() + terminal.rows() == pane.y() + pane.rows()
            })
            .collect()
    }
    fn panes_right_aligned_with_pane(&self, pane: &dyn Pane) -> Vec<&dyn Pane> {
        self.panes
            .keys()
            .map(|t_id| self.panes.get(t_id).unwrap().as_ref())
            .filter(|terminal| {
                terminal.pid() != pane.pid()
                    && terminal.x() + terminal.columns() == pane.x() + pane.columns()
            })
            .collect()
    }
    fn panes_left_aligned_with_pane(&self, pane: &dyn Pane) -> Vec<&dyn Pane> {
        self.panes
            .keys()
            .map(|t_id| self.panes.get(t_id).unwrap().as_ref())
            .filter(|terminal| terminal.pid() != pane.pid() && terminal.x() == pane.x())
            .collect()
    }
    fn right_aligned_contiguous_panes_above(
        &self,
        id: &PaneId,
        terminal_borders_to_the_right: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self
            .panes
            .get(id)
            .expect("terminal id does not exist")
            .as_ref();
        let mut right_aligned_terminals = self.panes_right_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        right_aligned_terminals.sort_by_key(|a| Reverse(a.y()));
        for terminal in right_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.y() + terminal.rows() == terminal_to_check.y() {
                terminals.push(terminal);
            }
        }
        // top-most border aligned with a pane border to the right
        let mut top_resize_border = 0;
        for terminal in &terminals {
            let bottom_terminal_boundary = terminal.y() + terminal.rows();
            if terminal_borders_to_the_right
                .get(&bottom_terminal_boundary)
                .is_some()
                && top_resize_border < bottom_terminal_boundary
            {
                top_resize_border = bottom_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.y() >= top_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let top_resize_border = if terminals.is_empty() {
            terminal_to_check.y()
        } else {
            top_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (top_resize_border, terminal_ids)
    }
    fn right_aligned_contiguous_panes_below(
        &self,
        id: &PaneId,
        terminal_borders_to_the_right: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self
            .panes
            .get(id)
            .expect("terminal id does not exist")
            .as_ref();
        let mut right_aligned_terminals = self.panes_right_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        right_aligned_terminals.sort_by_key(|a| a.y());
        for terminal in right_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.y() == terminal_to_check.y() + terminal_to_check.rows() {
                terminals.push(terminal);
            }
        }
        // bottom-most border aligned with a pane border to the right
        let mut bottom_resize_border = self.viewport.y + self.viewport.rows;
        for terminal in &terminals {
            let top_terminal_boundary = terminal.y();
            if terminal_borders_to_the_right
                .get(&(top_terminal_boundary))
                .is_some()
                && top_terminal_boundary < bottom_resize_border
            {
                bottom_resize_border = top_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.y() + terminal.rows() <= bottom_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let bottom_resize_border = if terminals.is_empty() {
            terminal_to_check.y() + terminal_to_check.rows()
        } else {
            bottom_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (bottom_resize_border, terminal_ids)
    }
    fn left_aligned_contiguous_panes_above(
        &self,
        id: &PaneId,
        terminal_borders_to_the_left: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self
            .panes
            .get(id)
            .expect("terminal id does not exist")
            .as_ref();
        let mut left_aligned_terminals = self.panes_left_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        left_aligned_terminals.sort_by_key(|a| Reverse(a.y()));
        for terminal in left_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.y() + terminal.rows() == terminal_to_check.y() {
                terminals.push(terminal);
            }
        }
        // top-most border aligned with a pane border to the right
        let mut top_resize_border = 0;
        for terminal in &terminals {
            let bottom_terminal_boundary = terminal.y() + terminal.rows();
            if terminal_borders_to_the_left
                .get(&bottom_terminal_boundary)
                .is_some()
                && top_resize_border < bottom_terminal_boundary
            {
                top_resize_border = bottom_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.y() >= top_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let top_resize_border = if terminals.is_empty() {
            terminal_to_check.y()
        } else {
            top_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (top_resize_border, terminal_ids)
    }
    fn left_aligned_contiguous_panes_below(
        &self,
        id: &PaneId,
        terminal_borders_to_the_left: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self
            .panes
            .get(id)
            .expect("terminal id does not exist")
            .as_ref();
        let mut left_aligned_terminals = self.panes_left_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        left_aligned_terminals.sort_by_key(|a| a.y());
        for terminal in left_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.y() == terminal_to_check.y() + terminal_to_check.rows() {
                terminals.push(terminal);
            }
        }
        // bottom-most border aligned with a pane border to the left
        let mut bottom_resize_border = self.viewport.y + self.viewport.rows;
        for terminal in &terminals {
            let top_terminal_boundary = terminal.y();
            if terminal_borders_to_the_left
                .get(&(top_terminal_boundary))
                .is_some()
                && top_terminal_boundary < bottom_resize_border
            {
                bottom_resize_border = top_terminal_boundary;
            }
        }
        terminals.retain(|terminal| {
            // terminal.y() + terminal.rows() < bottom_resize_border
            terminal.y() + terminal.rows() <= bottom_resize_border
        });
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let bottom_resize_border = if terminals.is_empty() {
            terminal_to_check.y() + terminal_to_check.rows()
        } else {
            bottom_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (bottom_resize_border, terminal_ids)
    }
    fn top_aligned_contiguous_panes_to_the_left(
        &self,
        id: &PaneId,
        terminal_borders_above: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self
            .panes
            .get(id)
            .expect("terminal id does not exist")
            .as_ref();
        let mut top_aligned_terminals = self.panes_top_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        top_aligned_terminals.sort_by_key(|a| Reverse(a.x()));
        for terminal in top_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.x() + terminal.columns() == terminal_to_check.x() {
                terminals.push(terminal);
            }
        }
        // leftmost border aligned with a pane border above
        let mut left_resize_border = 0;
        for terminal in &terminals {
            let right_terminal_boundary = terminal.x() + terminal.columns();
            if terminal_borders_above
                .get(&right_terminal_boundary)
                .is_some()
                && left_resize_border < right_terminal_boundary
            {
                left_resize_border = right_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.x() >= left_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let left_resize_border = if terminals.is_empty() {
            terminal_to_check.x()
        } else {
            left_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (left_resize_border, terminal_ids)
    }
    fn top_aligned_contiguous_panes_to_the_right(
        &self,
        id: &PaneId,
        terminal_borders_above: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self.panes.get(id).unwrap().as_ref();
        let mut top_aligned_terminals = self.panes_top_aligned_with_pane(terminal_to_check);
        // terminals that are next to each other up to current
        top_aligned_terminals.sort_by_key(|a| a.x());
        for terminal in top_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.x() == terminal_to_check.x() + terminal_to_check.columns() {
                terminals.push(terminal);
            }
        }
        // rightmost border aligned with a pane border above
        let mut right_resize_border = self.viewport.x + self.viewport.cols;
        for terminal in &terminals {
            let left_terminal_boundary = terminal.x();
            if terminal_borders_above
                .get(&left_terminal_boundary)
                .is_some()
                && right_resize_border > left_terminal_boundary
            {
                right_resize_border = left_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.x() + terminal.columns() <= right_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let right_resize_border = if terminals.is_empty() {
            terminal_to_check.x() + terminal_to_check.columns()
        } else {
            right_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (right_resize_border, terminal_ids)
    }
    fn bottom_aligned_contiguous_panes_to_the_left(
        &self,
        id: &PaneId,
        terminal_borders_below: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self.panes.get(id).unwrap().as_ref();
        let mut bottom_aligned_terminals = self.panes_bottom_aligned_with_pane(terminal_to_check);
        bottom_aligned_terminals.sort_by_key(|a| Reverse(a.x()));
        // terminals that are next to each other up to current
        for terminal in bottom_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.x() + terminal.columns() == terminal_to_check.x() {
                terminals.push(terminal);
            }
        }
        // leftmost border aligned with a pane border above
        let mut left_resize_border = 0;
        for terminal in &terminals {
            let right_terminal_boundary = terminal.x() + terminal.columns();
            if terminal_borders_below
                .get(&right_terminal_boundary)
                .is_some()
                && left_resize_border < right_terminal_boundary
            {
                left_resize_border = right_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.x() >= left_resize_border);
        // if there are no adjacent panes to resize, we use the border of the main pane we're
        // resizing
        let left_resize_border = if terminals.is_empty() {
            terminal_to_check.x()
        } else {
            left_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (left_resize_border, terminal_ids)
    }
    fn bottom_aligned_contiguous_panes_to_the_right(
        &self,
        id: &PaneId,
        terminal_borders_below: &HashSet<usize>,
    ) -> BorderAndPaneIds {
        let mut terminals = vec![];
        let terminal_to_check = self.panes.get(id).unwrap().as_ref();
        let mut bottom_aligned_terminals = self.panes_bottom_aligned_with_pane(terminal_to_check);
        bottom_aligned_terminals.sort_by_key(|a| a.x());
        // terminals that are next to each other up to current
        for terminal in bottom_aligned_terminals {
            let terminal_to_check = terminals.last().unwrap_or(&terminal_to_check);
            if terminal.x() == terminal_to_check.x() + terminal_to_check.columns() {
                terminals.push(terminal);
            }
        }
        // leftmost border aligned with a pane border above
        let mut right_resize_border = self.viewport.x + self.viewport.cols;
        for terminal in &terminals {
            let left_terminal_boundary = terminal.x();
            if terminal_borders_below
                .get(&left_terminal_boundary)
                .is_some()
                && right_resize_border > left_terminal_boundary
            {
                right_resize_border = left_terminal_boundary;
            }
        }
        terminals.retain(|terminal| terminal.x() + terminal.columns() <= right_resize_border);

        let right_resize_border = if terminals.is_empty() {
            terminal_to_check.x() + terminal_to_check.columns()
        } else {
            right_resize_border
        };
        let terminal_ids: Vec<PaneId> = terminals.iter().map(|t| t.pid()).collect();
        (right_resize_border, terminal_ids)
    }
    fn reduce_pane_height_down(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.reduce_height_down(count);
        let position_and_size = terminal.position_and_size();

        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = id {
            self.os_api.set_terminal_size_using_fd(
                *pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn reduce_pane_height_up(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.reduce_height_up(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = id {
            self.os_api.set_terminal_size_using_fd(
                *pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn increase_pane_height_down(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.increase_height_down(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn increase_pane_height_up(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.increase_height_up(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn increase_pane_width_right(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.increase_width_right(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn increase_pane_width_left(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.increase_width_left(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn reduce_pane_width_right(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.reduce_width_right(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn reduce_pane_width_left(&mut self, id: &PaneId, count: usize) {
        let terminal = self.panes.get_mut(id).unwrap();
        terminal.reduce_width_left(count);
        let position_and_size = terminal.position_and_size();
        if !self.draw_pane_frames {
            let (pane_columns_offset, pane_rows_offset) =
                pane_content_offset(&position_and_size, &self.viewport);
            terminal.offset_content_columns(pane_columns_offset);
            terminal.offset_content_rows(pane_rows_offset);
        }
        if let PaneId::Terminal(pid) = terminal.pid() {
            self.os_api.set_terminal_size_using_fd(
                pid,
                terminal.get_content_columns() as u16,
                terminal.get_content_rows() as u16,
            );
        }
    }
    fn pane_is_between_vertical_borders(
        &self,
        id: &PaneId,
        left_border_x: usize,
        right_border_x: usize,
    ) -> bool {
        let terminal = self
            .panes
            .get(id)
            .expect("could not find terminal to check between borders");
        terminal.x() >= left_border_x && terminal.x() + terminal.columns() <= right_border_x
    }
    fn pane_is_between_horizontal_borders(
        &self,
        id: &PaneId,
        top_border_y: usize,
        bottom_border_y: usize,
    ) -> bool {
        let terminal = self
            .panes
            .get(id)
            .expect("could not find terminal to check between borders");
        terminal.y() >= top_border_y && terminal.y() + terminal.rows() <= bottom_border_y
    }
    fn reduce_pane_and_surroundings_up(&mut self, id: &PaneId, count: usize) {
        let mut terminals_below = self
            .pane_ids_directly_below(id)
            .expect("can't reduce pane size up if there are no terminals below");
        let terminal_borders_below: HashSet<usize> = terminals_below
            .iter()
            .map(|t| self.panes.get(t).unwrap().x())
            .collect();
        let (left_resize_border, terminals_to_the_left) =
            self.bottom_aligned_contiguous_panes_to_the_left(id, &terminal_borders_below);
        let (right_resize_border, terminals_to_the_right) =
            self.bottom_aligned_contiguous_panes_to_the_right(id, &terminal_borders_below);
        terminals_below.retain(|t| {
            self.pane_is_between_vertical_borders(t, left_resize_border, right_resize_border)
        });

        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            let pane = self.panes.get(terminal_id).unwrap();
            if (pane.rows() as isize) - (count as isize) < pane.min_height() as isize {
                // dirty, dirty hack - should be fixed by the resizing overhaul
                return;
            }
        }

        self.reduce_pane_height_up(id, count);
        for terminal_id in terminals_below {
            self.increase_pane_height_up(&terminal_id, count);
        }
        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            self.reduce_pane_height_up(terminal_id, count);
        }
    }
    fn reduce_pane_and_surroundings_down(&mut self, id: &PaneId, count: usize) {
        let mut terminals_above = self
            .pane_ids_directly_above(id)
            .expect("can't reduce pane size down if there are no terminals above");
        let terminal_borders_above: HashSet<usize> = terminals_above
            .iter()
            .map(|t| self.panes.get(t).unwrap().x())
            .collect();
        let (left_resize_border, terminals_to_the_left) =
            self.top_aligned_contiguous_panes_to_the_left(id, &terminal_borders_above);
        let (right_resize_border, terminals_to_the_right) =
            self.top_aligned_contiguous_panes_to_the_right(id, &terminal_borders_above);
        terminals_above.retain(|t| {
            self.pane_is_between_vertical_borders(t, left_resize_border, right_resize_border)
        });

        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            let pane = self.panes.get(terminal_id).unwrap();
            if (pane.rows() as isize) - (count as isize) < pane.min_height() as isize {
                // dirty, dirty hack - should be fixed by the resizing overhaul
                return;
            }
        }

        self.reduce_pane_height_down(id, count);
        for terminal_id in terminals_above {
            self.increase_pane_height_down(&terminal_id, count);
        }
        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            self.reduce_pane_height_down(terminal_id, count);
        }
    }
    fn reduce_pane_and_surroundings_right(&mut self, id: &PaneId, count: usize) {
        let mut terminals_to_the_left = self
            .pane_ids_directly_left_of(id)
            .expect("can't reduce pane size right if there are no terminals to the left");
        let terminal_borders_to_the_left: HashSet<usize> = terminals_to_the_left
            .iter()
            .map(|t| self.panes.get(t).unwrap().y())
            .collect();
        let (top_resize_border, terminals_above) =
            self.left_aligned_contiguous_panes_above(id, &terminal_borders_to_the_left);
        let (bottom_resize_border, terminals_below) =
            self.left_aligned_contiguous_panes_below(id, &terminal_borders_to_the_left);
        terminals_to_the_left.retain(|t| {
            self.pane_is_between_horizontal_borders(t, top_resize_border, bottom_resize_border)
        });

        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            let pane = self.panes.get(terminal_id).unwrap();
            if (pane.columns() as isize) - (count as isize) < pane.min_width() as isize {
                // dirty, dirty hack - should be fixed by the resizing overhaul
                return;
            }
        }

        self.reduce_pane_width_right(id, count);
        for terminal_id in terminals_to_the_left {
            self.increase_pane_width_right(&terminal_id, count);
        }
        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            self.reduce_pane_width_right(terminal_id, count);
        }
    }
    fn reduce_pane_and_surroundings_left(&mut self, id: &PaneId, count: usize) {
        let mut terminals_to_the_right = self
            .pane_ids_directly_right_of(id)
            .expect("can't reduce pane size left if there are no terminals to the right");
        let terminal_borders_to_the_right: HashSet<usize> = terminals_to_the_right
            .iter()
            .map(|t| self.panes.get(t).unwrap().y())
            .collect();
        let (top_resize_border, terminals_above) =
            self.right_aligned_contiguous_panes_above(id, &terminal_borders_to_the_right);
        let (bottom_resize_border, terminals_below) =
            self.right_aligned_contiguous_panes_below(id, &terminal_borders_to_the_right);
        terminals_to_the_right.retain(|t| {
            self.pane_is_between_horizontal_borders(t, top_resize_border, bottom_resize_border)
        });

        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            let pane = self.panes.get(terminal_id).unwrap();
            if (pane.columns() as isize) - (count as isize) < pane.min_width() as isize {
                // dirty, dirty hack - should be fixed by the resizing overhaul
                return;
            }
        }

        self.reduce_pane_width_left(id, count);
        for terminal_id in terminals_to_the_right {
            self.increase_pane_width_left(&terminal_id, count);
        }
        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            self.reduce_pane_width_left(terminal_id, count);
        }
    }
    fn increase_pane_and_surroundings_up(&mut self, id: &PaneId, count: usize) {
        let mut terminals_above = self
            .pane_ids_directly_above(id)
            .expect("can't increase pane size up if there are no terminals above");
        let terminal_borders_above: HashSet<usize> = terminals_above
            .iter()
            .map(|t| self.panes.get(t).unwrap().x())
            .collect();
        let (left_resize_border, terminals_to_the_left) =
            self.top_aligned_contiguous_panes_to_the_left(id, &terminal_borders_above);
        let (right_resize_border, terminals_to_the_right) =
            self.top_aligned_contiguous_panes_to_the_right(id, &terminal_borders_above);
        terminals_above.retain(|t| {
            self.pane_is_between_vertical_borders(t, left_resize_border, right_resize_border)
        });
        self.increase_pane_height_up(id, count);
        for terminal_id in terminals_above {
            self.reduce_pane_height_up(&terminal_id, count);
        }
        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            self.increase_pane_height_up(terminal_id, count);
        }
    }
    fn increase_pane_and_surroundings_down(&mut self, id: &PaneId, count: usize) {
        let mut terminals_below = self
            .pane_ids_directly_below(id)
            .expect("can't increase pane size down if there are no terminals below");
        let terminal_borders_below: HashSet<usize> = terminals_below
            .iter()
            .map(|t| self.panes.get(t).unwrap().x())
            .collect();
        let (left_resize_border, terminals_to_the_left) =
            self.bottom_aligned_contiguous_panes_to_the_left(id, &terminal_borders_below);
        let (right_resize_border, terminals_to_the_right) =
            self.bottom_aligned_contiguous_panes_to_the_right(id, &terminal_borders_below);
        terminals_below.retain(|t| {
            self.pane_is_between_vertical_borders(t, left_resize_border, right_resize_border)
        });
        self.increase_pane_height_down(id, count);
        for terminal_id in terminals_below {
            self.reduce_pane_height_down(&terminal_id, count);
        }
        for terminal_id in terminals_to_the_left
            .iter()
            .chain(terminals_to_the_right.iter())
        {
            self.increase_pane_height_down(terminal_id, count);
        }
    }
    fn increase_pane_and_surroundings_right(&mut self, id: &PaneId, count: usize) {
        let mut terminals_to_the_right = self
            .pane_ids_directly_right_of(id)
            .expect("can't increase pane size right if there are no terminals to the right");
        let terminal_borders_to_the_right: HashSet<usize> = terminals_to_the_right
            .iter()
            .map(|t| {
                return self.panes.get(t).unwrap().y();
            })
            .collect();
        let (top_resize_border, terminals_above) =
            self.right_aligned_contiguous_panes_above(id, &terminal_borders_to_the_right);
        let (bottom_resize_border, terminals_below) =
            self.right_aligned_contiguous_panes_below(id, &terminal_borders_to_the_right);
        terminals_to_the_right.retain(|t| {
            self.pane_is_between_horizontal_borders(t, top_resize_border, bottom_resize_border)
        });
        self.increase_pane_width_right(id, count);
        for terminal_id in terminals_to_the_right {
            self.reduce_pane_width_right(&terminal_id, count);
        }
        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            self.increase_pane_width_right(terminal_id, count);
        }
    }
    fn increase_pane_and_surroundings_left(&mut self, id: &PaneId, count: usize) {
        let mut terminals_to_the_left = self
            .pane_ids_directly_left_of(id)
            .expect("can't increase pane size right if there are no terminals to the right");
        let terminal_borders_to_the_left: HashSet<usize> = terminals_to_the_left
            .iter()
            .map(|t| self.panes.get(t).unwrap().y())
            .collect();
        let (top_resize_border, terminals_above) =
            self.left_aligned_contiguous_panes_above(id, &terminal_borders_to_the_left);
        let (bottom_resize_border, terminals_below) =
            self.left_aligned_contiguous_panes_below(id, &terminal_borders_to_the_left);
        terminals_to_the_left.retain(|t| {
            self.pane_is_between_horizontal_borders(t, top_resize_border, bottom_resize_border)
        });
        self.increase_pane_width_left(id, count);
        for terminal_id in terminals_to_the_left {
            self.reduce_pane_width_left(&terminal_id, count);
        }
        for terminal_id in terminals_above.iter().chain(terminals_below.iter()) {
            self.increase_pane_width_left(terminal_id, count);
        }
    }
    fn can_increase_pane_and_surroundings_right(
        &self,
        pane_id: &PaneId,
        increase_by: usize,
    ) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let can_increase_pane_size = pane
            .max_width()
            .map(|max_width| pane.columns() + increase_by <= max_width)
            .unwrap_or(true); // no max width, increase to your heart's content
        if !can_increase_pane_size {
            return false;
        }
        let mut new_pos_and_size_for_pane = pane.position_and_size();
        new_pos_and_size_for_pane.cols += increase_by;

        if let Some(panes_to_the_right) = self.pane_ids_directly_right_of(pane_id) {
            return panes_to_the_right.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.columns() > increase_by && p.columns() - increase_by >= p.min_width()
            });
        } else {
            false
        }
    }
    fn can_increase_pane_and_surroundings_left(
        &self,
        pane_id: &PaneId,
        increase_by: usize,
    ) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let can_increase_pane_size = pane
            .max_width()
            .map(|max_width| pane.columns() + increase_by <= max_width)
            .unwrap_or(true); // no max width, increase to your heart's content
        if !can_increase_pane_size {
            return false;
        }
        let mut new_pos_and_size_for_pane = pane.position_and_size();
        new_pos_and_size_for_pane.x = new_pos_and_size_for_pane.x.saturating_sub(increase_by);

        if let Some(panes_to_the_left) = self.pane_ids_directly_left_of(pane_id) {
            return panes_to_the_left.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.columns() > increase_by && p.columns() - increase_by >= p.min_width()
            });
        } else {
            false
        }
    }
    fn can_increase_pane_and_surroundings_down(
        &self,
        pane_id: &PaneId,
        increase_by: usize,
    ) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let can_increase_pane_size = pane
            .max_height()
            .map(|max_height| pane.rows() + increase_by <= max_height)
            .unwrap_or(true); // no max width, increase to your heart's content
        if !can_increase_pane_size {
            return false;
        }
        let mut new_pos_and_size_for_pane = pane.position_and_size();
        new_pos_and_size_for_pane.rows += increase_by;

        if let Some(panes_below) = self.pane_ids_directly_below(pane_id) {
            return panes_below.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.rows() > increase_by && p.rows() - increase_by >= p.min_height()
            });
        } else {
            false
        }
    }
    fn can_increase_pane_and_surroundings_up(&self, pane_id: &PaneId, increase_by: usize) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let can_increase_pane_size = pane
            .max_height()
            .map(|max_height| pane.rows() + increase_by <= max_height)
            .unwrap_or(true); // no max width, increase to your heart's content
        if !can_increase_pane_size {
            return false;
        }
        let mut new_pos_and_size_for_pane = pane.position_and_size();
        new_pos_and_size_for_pane.y = new_pos_and_size_for_pane.y.saturating_sub(increase_by);

        if let Some(panes_above) = self.pane_ids_directly_above(pane_id) {
            return panes_above.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.rows() > increase_by && p.rows() - increase_by >= p.min_height()
            });
        } else {
            false
        }
    }
    fn can_reduce_pane_and_surroundings_right(&self, pane_id: &PaneId, reduce_by: usize) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let pane_columns = pane.columns();
        let can_reduce_pane_size =
            pane_columns > reduce_by && pane_columns - reduce_by >= pane.min_width();
        if !can_reduce_pane_size {
            return false;
        }
        if let Some(panes_to_the_left) = self.pane_ids_directly_left_of(pane_id) {
            return panes_to_the_left.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.max_width()
                    .map(|max_width| p.columns() + reduce_by <= max_width)
                    .unwrap_or(true) // no max width, increase to your heart's content
            });
        } else {
            false
        }
    }
    fn can_reduce_pane_and_surroundings_left(&self, pane_id: &PaneId, reduce_by: usize) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let pane_columns = pane.columns();
        let can_reduce_pane_size =
            pane_columns > reduce_by && pane_columns - reduce_by >= pane.min_width();
        if !can_reduce_pane_size {
            return false;
        }
        if let Some(panes_to_the_right) = self.pane_ids_directly_right_of(pane_id) {
            return panes_to_the_right.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.max_width()
                    .map(|max_width| p.columns() + reduce_by <= max_width)
                    .unwrap_or(true) // no max width, increase to your heart's content
            });
        } else {
            false
        }
    }
    fn can_reduce_pane_and_surroundings_down(&self, pane_id: &PaneId, reduce_by: usize) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let pane_rows = pane.rows();
        let can_reduce_pane_size =
            pane_rows > reduce_by && pane_rows - reduce_by >= pane.min_height();
        if !can_reduce_pane_size {
            return false;
        }
        if let Some(panes_above) = self.pane_ids_directly_above(pane_id) {
            return panes_above.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.max_height()
                    .map(|max_height| p.rows() + reduce_by <= max_height)
                    .unwrap_or(true) // no max height, increase to your heart's content
            });
        } else {
            false
        }
    }
    fn can_reduce_pane_and_surroundings_up(&self, pane_id: &PaneId, reduce_by: usize) -> bool {
        let pane = self.panes.get(pane_id).unwrap();
        let pane_rows = pane.rows();
        let can_reduce_pane_size =
            pane_rows > reduce_by && pane_rows - reduce_by >= pane.min_height();
        if !can_reduce_pane_size {
            return false;
        }
        if let Some(panes_below) = self.pane_ids_directly_below(pane_id) {
            return panes_below.iter().all(|id| {
                let p = self.panes.get(id).unwrap();
                p.max_height()
                    .map(|max_height| p.rows() + reduce_by <= max_height)
                    .unwrap_or(true) // no max height, increase to your heart's content
            });
        } else {
            false
        }
    }
    pub fn resize_whole_tab(&mut self, new_screen_size: PositionAndSize) {
        if self.fullscreen_is_active {
            // this is not ideal, we can improve this
            self.toggle_active_pane_fullscreen();
        }
        if let Some((column_difference, row_difference)) =
            PaneResizer::new(&mut self.panes, &mut self.os_api)
                .resize(self.display_area, new_screen_size)
        {
            self.should_clear_display_before_rendering = true;

            self.viewport.cols = (self.viewport.cols as isize + column_difference) as usize;
            self.viewport.rows = (self.viewport.rows as isize + row_difference) as usize;
            self.display_area.cols = (self.display_area.cols as isize + column_difference) as usize;
            self.display_area.rows = (self.display_area.rows as isize + row_difference) as usize;
        };
    }
    pub fn resize_left(&mut self) {
        // TODO: find out by how much we actually reduced and only reduce by that much
        let count = 10;
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if self.can_increase_pane_and_surroundings_left(&active_pane_id, count) {
                self.increase_pane_and_surroundings_left(&active_pane_id, count);
            } else if self.can_reduce_pane_and_surroundings_left(&active_pane_id, count) {
                self.reduce_pane_and_surroundings_left(&active_pane_id, count);
            }
        }
        self.render();
    }
    pub fn resize_right(&mut self) {
        // TODO: find out by how much we actually reduced and only reduce by that much
        let count = 10;
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if self.can_increase_pane_and_surroundings_right(&active_pane_id, count) {
                self.increase_pane_and_surroundings_right(&active_pane_id, count);
            } else if self.can_reduce_pane_and_surroundings_right(&active_pane_id, count) {
                self.reduce_pane_and_surroundings_right(&active_pane_id, count);
            }
        }
        self.render();
    }
    pub fn resize_down(&mut self) {
        // TODO: find out by how much we actually reduced and only reduce by that much
        let count = 2;
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if self.can_increase_pane_and_surroundings_down(&active_pane_id, count) {
                self.increase_pane_and_surroundings_down(&active_pane_id, count);
            } else if self.can_reduce_pane_and_surroundings_down(&active_pane_id, count) {
                self.reduce_pane_and_surroundings_down(&active_pane_id, count);
            }
        }
        self.render();
    }
    pub fn resize_up(&mut self) {
        // TODO: find out by how much we actually reduced and only reduce by that much
        let count = 2;
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if self.can_increase_pane_and_surroundings_up(&active_pane_id, count) {
                self.increase_pane_and_surroundings_up(&active_pane_id, count);
            } else if self.can_reduce_pane_and_surroundings_up(&active_pane_id, count) {
                self.reduce_pane_and_surroundings_up(&active_pane_id, count);
            }
        }
        self.render();
    }
    pub fn move_focus(&mut self) {
        if !self.has_selectable_panes() {
            return;
        }
        if self.fullscreen_is_active {
            return;
        }
        let active_terminal_id = self.get_active_pane_id().unwrap();
        let terminal_ids: Vec<PaneId> = self.get_selectable_panes().map(|(&pid, _)| pid).collect(); // TODO: better, no allocations
        let first_terminal = terminal_ids.get(0).unwrap();
        let active_terminal_id_position = terminal_ids
            .iter()
            .position(|id| id == &active_terminal_id)
            .unwrap();
        if let Some(next_terminal) = terminal_ids.get(active_terminal_id_position + 1) {
            self.active_terminal = Some(*next_terminal);
        } else {
            self.active_terminal = Some(*first_terminal);
        }
        self.render();
    }
    pub fn focus_next_pane(&mut self) {
        if !self.has_selectable_panes() {
            return;
        }
        if self.fullscreen_is_active {
            return;
        }
        let active_pane_id = self.get_active_pane_id().unwrap();
        let mut panes: Vec<(&PaneId, &Box<dyn Pane>)> = self.get_selectable_panes().collect();
        panes.sort_by(|(_a_id, a_pane), (_b_id, b_pane)| {
            if a_pane.y() == b_pane.y() {
                a_pane.x().cmp(&b_pane.x())
            } else {
                a_pane.y().cmp(&b_pane.y())
            }
        });
        let first_pane = panes.get(0).unwrap();
        let active_pane_position = panes
            .iter()
            .position(|(id, _)| *id == &active_pane_id) // TODO: better
            .unwrap();
        if let Some(next_pane) = panes.get(active_pane_position + 1) {
            self.active_terminal = Some(*next_pane.0);
        } else {
            self.active_terminal = Some(*first_pane.0);
        }
        self.render();
    }
    pub fn focus_previous_pane(&mut self) {
        if !self.has_selectable_panes() {
            return;
        }
        if self.fullscreen_is_active {
            return;
        }
        let active_pane_id = self.get_active_pane_id().unwrap();
        let mut panes: Vec<(&PaneId, &Box<dyn Pane>)> = self.get_selectable_panes().collect();
        panes.sort_by(|(_a_id, a_pane), (_b_id, b_pane)| {
            if a_pane.y() == b_pane.y() {
                a_pane.x().cmp(&b_pane.x())
            } else {
                a_pane.y().cmp(&b_pane.y())
            }
        });
        let last_pane = panes.last().unwrap();
        let active_pane_position = panes
            .iter()
            .position(|(id, _)| *id == &active_pane_id) // TODO: better
            .unwrap();
        if active_pane_position == 0 {
            self.active_terminal = Some(*last_pane.0);
        } else {
            self.active_terminal = Some(*panes.get(active_pane_position - 1).unwrap().0);
        }
        self.render();
    }
    // returns a boolean that indicates whether the focus moved
    pub fn move_focus_left(&mut self) -> bool {
        if !self.has_selectable_panes() {
            return false;
        }
        if self.fullscreen_is_active {
            return false;
        }
        let active_terminal = self.get_active_pane();
        if let Some(active) = active_terminal {
            let terminals = self.get_selectable_panes();
            let next_index = terminals
                .enumerate()
                .filter(|(_, (_, c))| {
                    c.is_directly_left_of(active) && c.horizontally_overlaps_with(active)
                })
                .max_by_key(|(_, (_, c))| c.active_at())
                .map(|(_, (pid, _))| pid);
            match next_index {
                Some(&p) => {
                    // render previously active pane so that its frame does not remain actively
                    // colored
                    let previously_active_pane =
                        self.panes.get_mut(&self.active_terminal.unwrap()).unwrap();
                    previously_active_pane.set_should_render(true);
                    let next_active_pane = self.panes.get_mut(&p).unwrap();
                    next_active_pane.set_should_render(true);

                    self.active_terminal = Some(p);
                    self.render();
                    return true;
                }
                None => {
                    self.active_terminal = Some(active.pid());
                }
            }
        } else {
            self.active_terminal = Some(active_terminal.unwrap().pid());
        }
        false
    }
    pub fn move_focus_down(&mut self) {
        if !self.has_selectable_panes() {
            return;
        }
        if self.fullscreen_is_active {
            return;
        }
        let active_terminal = self.get_active_pane();
        if let Some(active) = active_terminal {
            let terminals = self.get_selectable_panes();
            let next_index = terminals
                .enumerate()
                .filter(|(_, (_, c))| {
                    c.is_directly_below(active) && c.vertically_overlaps_with(active)
                })
                .max_by_key(|(_, (_, c))| c.active_at())
                .map(|(_, (pid, _))| pid);
            match next_index {
                Some(&p) => {
                    // render previously active pane so that its frame does not remain actively
                    // colored
                    let previously_active_pane =
                        self.panes.get_mut(&self.active_terminal.unwrap()).unwrap();
                    previously_active_pane.set_should_render(true);
                    let next_active_pane = self.panes.get_mut(&p).unwrap();
                    next_active_pane.set_should_render(true);

                    self.active_terminal = Some(p);
                }
                None => {
                    self.active_terminal = Some(active.pid());
                }
            }
        } else {
            self.active_terminal = Some(active_terminal.unwrap().pid());
        }
        self.render();
    }
    pub fn move_focus_up(&mut self) {
        if !self.has_selectable_panes() {
            return;
        }
        if self.fullscreen_is_active {
            return;
        }
        let active_terminal = self.get_active_pane();
        if let Some(active) = active_terminal {
            let terminals = self.get_selectable_panes();
            let next_index = terminals
                .enumerate()
                .filter(|(_, (_, c))| {
                    c.is_directly_above(active) && c.vertically_overlaps_with(active)
                })
                .max_by_key(|(_, (_, c))| c.active_at())
                .map(|(_, (pid, _))| pid);
            match next_index {
                Some(&p) => {
                    // render previously active pane so that its frame does not remain actively
                    // colored
                    let previously_active_pane =
                        self.panes.get_mut(&self.active_terminal.unwrap()).unwrap();
                    previously_active_pane.set_should_render(true);
                    let next_active_pane = self.panes.get_mut(&p).unwrap();
                    next_active_pane.set_should_render(true);

                    self.active_terminal = Some(p);
                }
                None => {
                    self.active_terminal = Some(active.pid());
                }
            }
        } else {
            self.active_terminal = Some(active_terminal.unwrap().pid());
        }
        self.render();
    }
    // returns a boolean that indicates whether the focus moved
    pub fn move_focus_right(&mut self) -> bool {
        if !self.has_selectable_panes() {
            return false;
        }
        if self.fullscreen_is_active {
            return false;
        }
        let active_terminal = self.get_active_pane();
        if let Some(active) = active_terminal {
            let terminals = self.get_selectable_panes();
            let next_index = terminals
                .enumerate()
                .filter(|(_, (_, c))| {
                    c.is_directly_right_of(active) && c.horizontally_overlaps_with(active)
                })
                .max_by_key(|(_, (_, c))| c.active_at())
                .map(|(_, (pid, _))| pid);
            match next_index {
                Some(&p) => {
                    // render previously active pane so that its frame does not remain actively
                    // colored
                    let previously_active_pane =
                        self.panes.get_mut(&self.active_terminal.unwrap()).unwrap();
                    previously_active_pane.set_should_render(true);
                    let next_active_pane = self.panes.get_mut(&p).unwrap();
                    next_active_pane.set_should_render(true);

                    self.active_terminal = Some(p);
                    self.render();
                    return true;
                }
                None => {
                    self.active_terminal = Some(active.pid());
                }
            }
        } else {
            self.active_terminal = Some(active_terminal.unwrap().pid());
        }
        false
    }
    fn horizontal_borders(&self, terminals: &[PaneId]) -> HashSet<usize> {
        terminals.iter().fold(HashSet::new(), |mut borders, t| {
            let terminal = self.panes.get(t).unwrap();
            borders.insert(terminal.y());
            borders.insert(terminal.y() + terminal.rows() + 1); // 1 for the border width
            borders
        })
    }
    fn vertical_borders(&self, terminals: &[PaneId]) -> HashSet<usize> {
        terminals.iter().fold(HashSet::new(), |mut borders, t| {
            let terminal = self.panes.get(t).unwrap();
            borders.insert(terminal.x());
            borders.insert(terminal.x() + terminal.columns() + 1); // 1 for the border width
            borders
        })
    }
    fn panes_to_the_left_between_aligning_borders(&self, id: PaneId) -> Option<Vec<PaneId>> {
        if let Some(terminal) = self.panes.get(&id) {
            let upper_close_border = terminal.y();
            let lower_close_border = terminal.y() + terminal.rows() + 1;

            if let Some(mut terminals_to_the_left) = self.pane_ids_directly_left_of(&id) {
                let terminal_borders_to_the_left = self.horizontal_borders(&terminals_to_the_left);
                if terminal_borders_to_the_left.contains(&upper_close_border)
                    && terminal_borders_to_the_left.contains(&lower_close_border)
                {
                    terminals_to_the_left.retain(|t| {
                        self.pane_is_between_horizontal_borders(
                            t,
                            upper_close_border,
                            lower_close_border,
                        )
                    });
                    return Some(terminals_to_the_left);
                }
            }
        }
        None
    }
    fn panes_to_the_right_between_aligning_borders(&self, id: PaneId) -> Option<Vec<PaneId>> {
        if let Some(terminal) = self.panes.get(&id) {
            let upper_close_border = terminal.y();
            let lower_close_border = terminal.y() + terminal.rows() + 1;

            if let Some(mut terminals_to_the_right) = self.pane_ids_directly_right_of(&id) {
                let terminal_borders_to_the_right =
                    self.horizontal_borders(&terminals_to_the_right);
                if terminal_borders_to_the_right.contains(&upper_close_border)
                    && terminal_borders_to_the_right.contains(&lower_close_border)
                {
                    terminals_to_the_right.retain(|t| {
                        self.pane_is_between_horizontal_borders(
                            t,
                            upper_close_border,
                            lower_close_border,
                        )
                    });
                    return Some(terminals_to_the_right);
                }
            }
        }
        None
    }
    fn panes_above_between_aligning_borders(&self, id: PaneId) -> Option<Vec<PaneId>> {
        if let Some(terminal) = self.panes.get(&id) {
            let left_close_border = terminal.x();
            let right_close_border = terminal.x() + terminal.columns() + 1;

            if let Some(mut terminals_above) = self.pane_ids_directly_above(&id) {
                let terminal_borders_above = self.vertical_borders(&terminals_above);
                if terminal_borders_above.contains(&left_close_border)
                    && terminal_borders_above.contains(&right_close_border)
                {
                    terminals_above.retain(|t| {
                        self.pane_is_between_vertical_borders(
                            t,
                            left_close_border,
                            right_close_border,
                        )
                    });
                    return Some(terminals_above);
                }
            }
        }
        None
    }
    fn panes_below_between_aligning_borders(&self, id: PaneId) -> Option<Vec<PaneId>> {
        if let Some(terminal) = self.panes.get(&id) {
            let left_close_border = terminal.x();
            let right_close_border = terminal.x() + terminal.columns() + 1;

            if let Some(mut terminals_below) = self.pane_ids_directly_below(&id) {
                let terminal_borders_below = self.vertical_borders(&terminals_below);
                if terminal_borders_below.contains(&left_close_border)
                    && terminal_borders_below.contains(&right_close_border)
                {
                    terminals_below.retain(|t| {
                        self.pane_is_between_vertical_borders(
                            t,
                            left_close_border,
                            right_close_border,
                        )
                    });
                    return Some(terminals_below);
                }
            }
        }
        None
    }
    fn close_down_to_max_terminals(&mut self) {
        if let Some(max_panes) = self.max_panes {
            let terminals = self.get_pane_ids();
            for &pid in terminals.iter().skip(max_panes - 1) {
                self.senders
                    .send_to_pty(PtyInstruction::ClosePane(pid))
                    .unwrap();
                self.close_pane_without_rerender(pid);
            }
        }
    }
    pub fn get_pane_ids(&self) -> Vec<PaneId> {
        self.get_panes().map(|(&pid, _)| pid).collect()
    }
    pub fn set_pane_selectable(&mut self, id: PaneId, selectable: bool) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.set_selectable(selectable);
            if self.get_active_pane_id() == Some(id) && !selectable {
                self.active_terminal = self.next_active_pane(&self.get_pane_ids())
            }
        }
    }
    pub fn set_pane_invisible_borders(&mut self, id: PaneId, invisible_borders: bool) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.set_invisible_borders(invisible_borders);
        }
    }
    pub fn set_pane_fixed_height(&mut self, id: PaneId, fixed_height: usize) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.set_fixed_height(fixed_height);
        }
    }
    pub fn set_pane_fixed_width(&mut self, id: PaneId, fixed_width: usize) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.set_fixed_width(fixed_width);
        }
    }
    pub fn close_pane(&mut self, id: PaneId) {
        if self.panes.get(&id).is_some() {
            self.close_pane_without_rerender(id);
        }
    }
    pub fn close_pane_without_rerender(&mut self, id: PaneId) {
        if self.fullscreen_is_active {
            self.toggle_active_pane_fullscreen();
        }
        if let Some(pane_to_close) = self.panes.get(&id) {
            let pane_to_close_width = pane_to_close.columns();
            let pane_to_close_height = pane_to_close.rows();
            if let Some(panes) = self.panes_to_the_left_between_aligning_borders(id) {
                if panes.iter().all(|p| {
                    let pane = self.panes.get(p).unwrap();
                    pane.can_increase_width_by(pane_to_close_width)
                }) {
                    self.panes.remove(&id);
                    if self.active_terminal == Some(id) {
                        let next_active_pane = self.next_active_pane(&panes);
                        self.active_terminal = next_active_pane;
                        if let Some(next_active_pane) = next_active_pane {
                            if self.is_the_only_selectable_pane(&next_active_pane)
                                && self.draw_pane_frames
                            {
                                let should_render_only_title = true;
                                self.panes
                                    .get_mut(&next_active_pane)
                                    .unwrap()
                                    .show_boundaries_frame(should_render_only_title);
                            }
                        }
                    }
                    for pane_id in panes.iter() {
                        self.increase_pane_width_right(pane_id, pane_to_close_width);
                    }
                    return;
                }
            }
            if let Some(panes) = self.panes_to_the_right_between_aligning_borders(id) {
                if panes.iter().all(|p| {
                    let pane = self.panes.get(p).unwrap();
                    pane.can_increase_width_by(pane_to_close_width)
                }) {
                    self.panes.remove(&id);
                    if self.active_terminal == Some(id) {
                        let next_active_pane = self.next_active_pane(&panes);
                        self.active_terminal = next_active_pane;
                        if let Some(next_active_pane) = next_active_pane {
                            if self.is_the_only_selectable_pane(&next_active_pane)
                                && self.draw_pane_frames
                            {
                                let should_render_only_title = true;
                                self.panes
                                    .get_mut(&next_active_pane)
                                    .unwrap()
                                    .show_boundaries_frame(should_render_only_title);
                            }
                        }
                    }
                    for pane_id in panes.iter() {
                        self.increase_pane_width_left(pane_id, pane_to_close_width);
                    }
                    return;
                }
            }
            if let Some(panes) = self.panes_above_between_aligning_borders(id) {
                if panes.iter().all(|p| {
                    let pane = self.panes.get(p).unwrap();
                    pane.can_increase_height_by(pane_to_close_height)
                }) {
                    self.panes.remove(&id);
                    if self.active_terminal == Some(id) {
                        let next_active_pane = self.next_active_pane(&panes);
                        self.active_terminal = next_active_pane;
                        if let Some(next_active_pane) = next_active_pane {
                            if self.is_the_only_selectable_pane(&next_active_pane)
                                && self.draw_pane_frames
                            {
                                let should_render_only_title = true;
                                self.panes
                                    .get_mut(&next_active_pane)
                                    .unwrap()
                                    .show_boundaries_frame(should_render_only_title);
                            }
                        }
                    }
                    for pane_id in panes.iter() {
                        self.increase_pane_height_down(pane_id, pane_to_close_height);
                    }
                    return;
                }
            }
            if let Some(panes) = self.panes_below_between_aligning_borders(id) {
                if panes.iter().all(|p| {
                    let pane = self.panes.get(p).unwrap();
                    pane.can_increase_height_by(pane_to_close_height)
                }) {
                    self.panes.remove(&id);
                    if self.active_terminal == Some(id) {
                        let next_active_pane = self.next_active_pane(&panes);
                        self.active_terminal = next_active_pane;
                        if let Some(next_active_pane) = next_active_pane {
                            if self.is_the_only_selectable_pane(&next_active_pane)
                                && self.draw_pane_frames
                            {
                                let should_render_only_title = true;
                                self.panes
                                    .get_mut(&next_active_pane)
                                    .unwrap()
                                    .show_boundaries_frame(should_render_only_title);
                            }
                        }
                    }
                    for pane_id in panes.iter() {
                        self.increase_pane_height_up(pane_id, pane_to_close_height);
                    }
                    return;
                }
            }
            // if we reached here, this is either the last pane or there's some sort of
            // configuration error (eg. we're trying to close a pane surrounded by fixed panes)
            self.panes.remove(&id);
        }
    }
    pub fn close_focused_pane(&mut self) {
        if let Some(active_pane_id) = self.get_active_pane_id() {
            self.close_pane(active_pane_id);
            self.senders
                .send_to_pty(PtyInstruction::ClosePane(active_pane_id))
                .unwrap();
        }
    }
    pub fn scroll_active_terminal_up(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            active_terminal.scroll_up(1);
            self.render();
        }
    }
    pub fn scroll_active_terminal_down(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            active_terminal.scroll_down(1);
            self.render();
        }
    }
    pub fn scroll_active_terminal_up_page(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            // prevent overflow when row == 0
            let scroll_columns = active_terminal.rows().max(1) - 1;
            active_terminal.scroll_up(scroll_columns);
            self.render();
        }
    }
    pub fn scroll_active_terminal_down_page(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            // prevent overflow when row == 0
            let scroll_columns = active_terminal.rows().max(1) - 1;
            active_terminal.scroll_down(scroll_columns);
            self.render();
        }
    }
    pub fn scroll_active_terminal_to_bottom(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            active_terminal.clear_scroll();
            self.render();
        }
    }
    pub fn clear_active_terminal_scroll(&mut self) {
        if let Some(active_terminal_id) = self.get_active_terminal_id() {
            let active_terminal = self
                .panes
                .get_mut(&PaneId::Terminal(active_terminal_id))
                .unwrap();
            active_terminal.clear_scroll();
        }
    }
    pub fn scroll_terminal_up(&mut self, point: &Position, lines: usize) {
        if let Some(pane) = self.get_pane_at(point) {
            pane.scroll_up(lines);
            self.render();
        }
    }
    pub fn scroll_terminal_down(&mut self, point: &Position, lines: usize) {
        if let Some(pane) = self.get_pane_at(point) {
            pane.scroll_down(lines);
            self.render();
        }
    }
    fn get_pane_at(&mut self, point: &Position) -> Option<&mut Box<dyn Pane>> {
        if let Some(pane_id) = self.get_pane_id_at(point) {
            self.panes.get_mut(&pane_id)
        } else {
            None
        }
    }
    fn get_pane_id_at(&self, point: &Position) -> Option<PaneId> {
        if self.fullscreen_is_active {
            return self.get_active_pane_id();
        }

        self.get_selectable_panes()
            .find(|(_, p)| p.contains(point))
            .map(|(&id, _)| id)
    }
    pub fn handle_left_click(&mut self, position: &Position) {
        self.focus_pane_at(position);

        if let Some(pane) = self.get_pane_at(position) {
            let relative_position = pane.relative_position(position);
            pane.start_selection(&relative_position);
            self.render();
        };
    }
    fn focus_pane_at(&mut self, point: &Position) {
        if let Some(clicked_pane) = self.get_pane_id_at(point) {
            self.active_terminal = Some(clicked_pane);
            self.render();
        }
    }
    pub fn handle_mouse_release(&mut self, position: &Position) {
        let active_pane_id = self.get_active_pane_id();
        // on release, get the selected text from the active pane, and reset it's selection
        let mut selected_text = None;
        if active_pane_id != self.get_pane_id_at(position) {
            if let Some(active_pane_id) = active_pane_id {
                if let Some(active_pane) = self.panes.get_mut(&active_pane_id) {
                    active_pane.end_selection(None);
                    selected_text = active_pane.get_selected_text();
                    active_pane.reset_selection();
                    self.render();
                }
            }
        } else if let Some(pane) = self.get_pane_at(position) {
            let relative_position = pane.relative_position(position);
            pane.end_selection(Some(&relative_position));
            selected_text = pane.get_selected_text();
            pane.reset_selection();
            self.render();
        }

        if let Some(selected_text) = selected_text {
            self.write_selection_to_clipboard(&selected_text);
        }
    }
    pub fn handle_mouse_hold(&mut self, position_on_screen: &Position) {
        if let Some(active_pane_id) = self.get_active_pane_id() {
            if let Some(active_pane) = self.panes.get_mut(&active_pane_id) {
                let relative_position = active_pane.relative_position(position_on_screen);
                active_pane.update_selection(&relative_position);
            }
        }
        self.render();
    }

    pub fn copy_selection(&self) {
        let selected_text = self.get_active_pane().and_then(|p| p.get_selected_text());
        if let Some(selected_text) = selected_text {
            self.write_selection_to_clipboard(&selected_text);
        }
    }

    fn write_selection_to_clipboard(&self, selection: &str) {
        let output = format!("\u{1b}]52;c;{}\u{1b}\\", base64::encode(selection));
        self.senders
            .send_to_server(ServerInstruction::Render(Some(output)))
            .unwrap();
    }
    fn is_inside_viewport(&self, pane_id: &PaneId) -> bool {
        let pane_position_and_size = self.panes.get(pane_id).unwrap().position_and_size();
        pane_position_and_size.y >= self.viewport.y
            && pane_position_and_size.y + pane_position_and_size.rows
                <= self.viewport.y + self.viewport.rows
    }
    fn offset_viewport(&mut self, position_and_size: &PositionAndSize) {
        if position_and_size.x == self.viewport.x
            && position_and_size.x + position_and_size.cols == self.viewport.x + self.viewport.cols
        {
            if position_and_size.y == self.viewport.y {
                self.viewport.y += position_and_size.rows;
                self.viewport.rows -= position_and_size.rows;
            } else if position_and_size.y + position_and_size.rows
                == self.viewport.y + self.viewport.rows
            {
                self.viewport.rows -= position_and_size.rows;
            }
        }
        if position_and_size.y == self.viewport.y
            && position_and_size.y + position_and_size.rows == self.viewport.y + self.viewport.rows
        {
            if position_and_size.x == self.viewport.x {
                self.viewport.x += position_and_size.cols;
                self.viewport.cols -= position_and_size.cols;
            } else if position_and_size.x + position_and_size.cols
                == self.viewport.x + self.viewport.cols
            {
                self.viewport.cols -= position_and_size.cols;
            }
        }
    }
}

#[cfg(test)]
#[path = "./unit/tab_tests.rs"]
mod tab_tests;
