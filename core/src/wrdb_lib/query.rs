use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryModifiers {
    pub order_by: Option<String>,
    pub order_direction: Option<OrderDirection>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}
