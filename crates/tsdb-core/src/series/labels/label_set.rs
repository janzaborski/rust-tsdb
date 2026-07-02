use super::label::Label;

#[derive(Clone, Hash, Eq, PartialEq)]
pub struct LabelSet(Vec<Label>);
