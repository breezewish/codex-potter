use std::collections::VecDeque;

use ratatui::text::Line;

use crate::markdown_stream::MarkdownStreamCollector;
pub mod controller;

pub struct StreamState {
    pub collector: MarkdownStreamCollector,
    queued_lines: VecDeque<Line<'static>>,
    pub has_seen_delta: bool,
}

impl StreamState {
    pub fn new(width: Option<usize>) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
        }
    }
    pub fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
    }
    pub fn step(&mut self) -> Vec<Line<'static>> {
        self.queued_lines.pop_front().into_iter().collect()
    }
    pub fn drain_all(&mut self) -> Vec<Line<'static>> {
        self.queued_lines.drain(..).collect()
    }
    pub fn is_idle(&self) -> bool {
        self.queued_lines.is_empty()
    }
    pub fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        self.queued_lines.extend(lines);
    }
}
