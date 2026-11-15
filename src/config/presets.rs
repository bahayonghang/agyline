use crate::config::ItemId;

pub fn default_cometix_lines() -> Vec<Vec<ItemId>> {
    vec![
        vec![
            ItemId::Model,
            ItemId::Dir,
            ItemId::Git,
            ItemId::Context,
            ItemId::Cache,
            ItemId::Speed,
            ItemId::Budget,
        ],
        vec![
            ItemId::Mode,
            ItemId::Subagents,
            ItemId::Tasks,
            ItemId::Queue,
            ItemId::Sandbox,
        ],
    ]
}
