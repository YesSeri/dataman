use crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl From<KeyCode> for Direction {
    fn from(value: KeyCode) -> Self {
        match value {
            KeyCode::Right => Direction::Right,
            KeyCode::Left => Direction::Left,
            KeyCode::Up => Direction::Up,
            KeyCode::Down => Direction::Down,
            _ => unreachable!(),
        }
    }
}
