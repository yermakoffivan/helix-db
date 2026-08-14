//! Executable return-shape contracts.
//!
//! Request returns remain a list of names. This module resolves each name to
//! the semantic output shape of its binding after executable lowering. Runtime
//! code therefore never infers shape from observed rows or cost estimates.

use std::collections::BTreeSet;

use crate::ir;

use super::{ExecOp, ExecPlanError, ExecStep};

/// Shape used only when a declared return has no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnShape {
    /// A collection return serializes an empty value as `[]`.
    List,
    /// An at-most-one return serializes an empty value as `null`.
    Object,
    /// A scalar return has no synthetic empty representation.
    Scalar,
}

/// One executable return with its planner-inferred shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableReturn {
    name: ir::NonEmptyString,
    shape: ReturnShape,
}

impl ExecutableReturn {
    /// Build one executable return.
    ///
    /// ```
    /// use helix_planner::{exec, ir};
    ///
    /// let returned = exec::ExecutableReturn::new(
    ///     ir::NonEmptyString::from_static("user"),
    ///     exec::ReturnShape::Object,
    /// );
    /// assert_eq!(returned.name().as_ref(), "user");
    /// assert_eq!(returned.shape(), exec::ReturnShape::Object);
    /// ```
    pub fn new(name: ir::NonEmptyString, shape: ReturnShape) -> Self {
        Self { name, shape }
    }

    /// Returned variable name.
    pub const fn name(&self) -> &ir::NonEmptyString {
        &self.name
    }

    /// Planner-inferred output shape.
    pub const fn shape(&self) -> ReturnShape {
        self.shape
    }
}

/// Non-empty executable returns with unique names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableReturnVariables {
    returns: ir::AtLeast<ExecutableReturn, 1>,
}

impl ExecutableReturnVariables {
    /// Build executable returns, rejecting duplicate names.
    pub fn new(
        returns: ir::AtLeast<ExecutableReturn, 1>,
    ) -> Result<Self, ir::ReturnVariablesError> {
        let mut names = BTreeSet::new();
        for returned in &returns {
            if !names.insert(returned.name.clone()) {
                return Err(ir::ReturnVariablesError::DuplicateName {
                    name: returned.name.clone(),
                });
            }
        }
        Ok(Self { returns })
    }
}

impl AsRef<[ExecutableReturn]> for ExecutableReturnVariables {
    fn as_ref(&self) -> &[ExecutableReturn] {
        self.returns.as_ref()
    }
}

/// Resolved executable returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutableReturns {
    /// Return no variables.
    None,
    /// Return one or more shaped variables.
    Variables(ExecutableReturnVariables),
}

impl ExecutableReturns {
    pub(super) fn resolve(
        requested: &ir::ReturnPlan,
        steps: &[ExecStep],
    ) -> Result<Self, ExecPlanError> {
        let ir::ReturnPlan::Variables(variables) = requested else {
            return Ok(Self::None);
        };
        let returns = variables
            .as_ref()
            .iter()
            .map(|name| {
                return_shape(steps, name)
                    .map(|shape| ExecutableReturn::new(name.clone(), shape))
                    .ok_or_else(|| ExecPlanError::MissingReturnBinding { name: name.clone() })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::Variables(
            ExecutableReturnVariables::new(
                ir::AtLeast::try_from_vec(returns)
                    .expect("ReturnPlan::Variables always contains at least one name"),
            )
            .expect("ReturnPlan::Variables already guarantees unique names"),
        ))
    }
}

fn return_shape(steps: &[ExecStep], name: &ir::NonEmptyString) -> Option<ReturnShape> {
    steps.iter().rev().find_map(|step| {
        if matches!(&step.output, ir::BatchOutputPlan::Bind(output) if output == name) {
            return Some(step_shape(step));
        }
        match &step.op {
            ExecOp::ForEach { body, .. } => return_shape(body.steps(), name),
            _ => None,
        }
    })
}

fn step_shape(step: &ExecStep) -> ReturnShape {
    match &step.op {
        ExecOp::Project {
            projection: ir::ProjectionPlan::Count | ir::ProjectionPlan::Exists,
        }
        | ExecOp::IndexDdl { .. } => ReturnShape::Scalar,
        ExecOp::Reserved {
            op: ir::ReservedOp::Fold,
        } => ReturnShape::List,
        ExecOp::Mutation { .. } => ReturnShape::List,
        _ => match step.delivered.cardinality.upper() {
            Some(0 | 1) => ReturnShape::Object,
            Some(_) | None => ReturnShape::List,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{ExecCondition, ExecSchedule, ExecStepId};
    use crate::{cost, properties};

    fn name(value: &str) -> ir::NonEmptyString {
        ir::NonEmptyString::new(value).unwrap()
    }

    fn step(op: ExecOp, cardinality: properties::CardinalityBounds) -> ExecStep {
        ExecStep {
            id: ExecStepId::new(1).unwrap(),
            dependencies: Vec::new(),
            output: ir::BatchOutputPlan::Bind(name("result")),
            condition: ExecCondition::Always,
            op,
            schedule: ExecSchedule::Pipeline,
            delivered: properties::DeliveredProperties {
                cardinality,
                ..properties::DeliveredProperties::default()
            },
            cost: cost::CostVector::ZERO,
        }
    }

    #[test]
    fn shape_depends_on_semantics_not_selected_operator_or_cost() {
        let point = step(
            ExecOp::Noop,
            properties::CardinalityBounds::zero_to(Some(1)),
        );
        let mut alternative = step(
            ExecOp::Barrier {
                name: name("physical-alternative"),
            },
            properties::CardinalityBounds::zero_to(Some(1)),
        );
        alternative.cost = cost::CostVector {
            object_reads: u64::MAX,
            cpu_units: u64::MAX,
            ..cost::CostVector::ZERO
        };
        let bounded_collection = step(
            ExecOp::Noop,
            properties::CardinalityBounds::zero_to(Some(2)),
        );
        let unknown_collection = step(ExecOp::Noop, properties::CardinalityBounds::unknown());

        assert_eq!(step_shape(&point), ReturnShape::Object);
        assert_eq!(step_shape(&alternative), ReturnShape::Object);
        assert_eq!(step_shape(&bounded_collection), ReturnShape::List);
        assert_eq!(step_shape(&unknown_collection), ReturnShape::List);
    }

    #[test]
    fn scalar_and_collection_terminals_override_row_cardinality() {
        let count = step(
            ExecOp::Project {
                projection: ir::ProjectionPlan::Count,
            },
            properties::CardinalityBounds::exact(1),
        );
        let exists = step(
            ExecOp::Project {
                projection: ir::ProjectionPlan::Exists,
            },
            properties::CardinalityBounds::exact(1),
        );
        let index_ddl = step(
            ExecOp::IndexDdl {
                plan: ir::IndexDdlPlan::GetOperation {
                    operation_id: ir::IndexOperationId::try_new(
                        "07070707-0707-0707-0707-070707070707",
                    )
                    .unwrap(),
                },
            },
            properties::CardinalityBounds::exact(1),
        );
        let fold = step(
            ExecOp::Reserved {
                op: ir::ReservedOp::Fold,
            },
            properties::CardinalityBounds::zero_to(Some(1)),
        );

        assert_eq!(step_shape(&count), ReturnShape::Scalar);
        assert_eq!(step_shape(&exists), ReturnShape::Scalar);
        assert_eq!(step_shape(&index_ddl), ReturnShape::Scalar);
        assert_eq!(step_shape(&fold), ReturnShape::List);
    }

    #[test]
    fn mutations_keep_the_existing_list_empty_shape() {
        let mutation = step(
            ExecOp::Mutation {
                plan: super::super::ExecMutationPlan::Drop,
            },
            properties::CardinalityBounds::zero_to(Some(1)),
        );

        assert_eq!(step_shape(&mutation), ReturnShape::List);
    }

    #[test]
    fn executable_return_variables_reject_duplicate_names() {
        let returned = ExecutableReturn::new(name("result"), ReturnShape::List);
        assert_eq!(returned.name().as_ref(), "result");
        let duplicate = ir::AtLeast::from_one_and_rest(
            returned,
            vec![ExecutableReturn::new(name("result"), ReturnShape::Object)],
        );

        assert!(matches!(
            ExecutableReturnVariables::new(duplicate),
            Err(ir::ReturnVariablesError::DuplicateName { .. })
        ));
    }

    #[test]
    fn return_resolution_rejects_names_without_executable_bindings() {
        let requested = ir::ReturnPlan::Variables(
            ir::ReturnVariables::new(ir::AtLeast::from_one(name("missing"))).unwrap(),
        );

        assert_eq!(
            ExecutableReturns::resolve(
                &requested,
                &[step(ExecOp::Noop, properties::CardinalityBounds::unknown())],
            ),
            Err(ExecPlanError::MissingReturnBinding {
                name: name("missing")
            })
        );
    }

    #[test]
    fn empty_return_declaration_resolves_without_steps() {
        assert_eq!(
            ExecutableReturns::resolve(&ir::ReturnPlan::None, &[]),
            Ok(ExecutableReturns::None)
        );
    }

    #[test]
    fn return_resolution_finds_bindings_inside_foreach_bodies() {
        let body_step = step(
            ExecOp::Noop,
            properties::CardinalityBounds::zero_to(Some(1)),
        );
        let body = super::super::ExecutableSubplan::new(
            ir::AtLeast::from_one(body_step),
            ExecStepId::new(1).unwrap(),
        )
        .unwrap();
        let mut foreach = step(
            ExecOp::ForEach {
                param: name("items"),
                body: Box::new(body),
            },
            properties::CardinalityBounds::unknown(),
        );
        foreach.output = ir::BatchOutputPlan::Discard;
        let requested = ir::ReturnPlan::Variables(
            ir::ReturnVariables::new(ir::AtLeast::from_one(name("result"))).unwrap(),
        );

        let expected = ExecutableReturns::Variables(
            ExecutableReturnVariables::new(ir::AtLeast::from_one(ExecutableReturn::new(
                name("result"),
                ReturnShape::Object,
            )))
            .unwrap(),
        );

        assert_eq!(
            ExecutableReturns::resolve(&requested, &[foreach]),
            Ok(expected)
        );
    }
}
