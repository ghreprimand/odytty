use crate::core::{Snapshot, TerminalModel};

pub trait Renderer {
    fn draw(&mut self, snapshot: &Snapshot);
}

#[derive(Debug, Default)]
pub struct NullRenderer {
    pub frames_drawn: usize,
}

impl Renderer for NullRenderer {
    fn draw(&mut self, _snapshot: &Snapshot) {
        self.frames_drawn += 1;
    }
}

impl NullRenderer {
    pub fn draw_model(&mut self, model: &impl TerminalModel) {
        self.draw(&model.snapshot());
    }
}
