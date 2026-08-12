//! One-to-one lowering for selected exact count programs.

use super::*;

impl ExecutableDagBuilder<'_> {
    pub(in crate::exec::selected::lowering) fn push_selected_count_root(
        &mut self,
        count: SelectedRootCount,
        dependencies: Vec<ExecStepId>,
        output: ir::BatchOutputPlan,
        condition: ExecCondition,
    ) -> Result<ExecStepId, ExecPlanError> {
        let (alternative, _provenance, input, plan) = count.into_parts();
        let dependencies = match input {
            SelectedCountInput::Direct => dependencies,
            SelectedCountInput::Rows(root) | SelectedCountInput::Scalars(root) => {
                let input = self.push_selected_run_root(
                    *root,
                    dependencies,
                    ir::BatchOutputPlan::Discard,
                    condition.clone(),
                )?;
                vec![input]
            }
        };
        let (delivered, cost) = alternative.clone_contract();
        self.push_step(StepDraft {
            dependencies,
            output,
            condition,
            op: ExecOp::Count {
                plan: Box::new(plan),
            },
            schedule: ExecSchedule::Barrier,
            delivered,
            cost,
        })
    }
}
