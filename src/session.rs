use crate::transform::SessionEvent;

pub trait Session {
    fn id(&self) -> &str;
    fn timestamp(&self) -> &str;
    fn cwd(&self) -> Option<&str>;
    fn events(&self) -> &[SessionEvent];
}
