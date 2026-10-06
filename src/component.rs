use crossterm::event::Event;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub trait Component {
    type State;

    fn handle_event(self, event: &Event, state: &mut Self::State);
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State);
}
