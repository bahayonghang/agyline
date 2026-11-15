use crate::collect::FrameData;
use crate::config::{EffectiveConfig, ItemId};
use crate::render::StyledItem;

pub mod runtime;
pub mod usage;
pub mod workspace;

pub fn evaluate_item(
    id: ItemId,
    frame: &FrameData,
    config: &EffectiveConfig,
) -> Option<StyledItem> {
    match id {
        ItemId::Model => workspace::eval_model(frame, config),
        ItemId::Dir => workspace::eval_dir(frame, config),
        ItemId::Git => workspace::eval_git(frame, config),
        ItemId::Context => usage::eval_context(frame, config),
        ItemId::Cache => usage::eval_cache(frame, config),
        ItemId::Speed => usage::eval_speed(frame, config),
        ItemId::Budget => usage::eval_budget(frame, config),
        ItemId::Mode => runtime::eval_mode(frame, config),
        ItemId::Subagents => runtime::eval_subagents(frame, config),
        ItemId::Tasks => runtime::eval_tasks(frame, config),
        ItemId::Queue => runtime::eval_queue(frame, config),
        ItemId::Sandbox => runtime::eval_sandbox(frame, config),
    }
}
