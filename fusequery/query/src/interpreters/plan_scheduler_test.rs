// Copyright 2020-2021 The Datafuse Authors.
//
// SPDX-License-Identifier: Apache-2.0.

use std::sync::Arc;

use common_datavalues::DataSchema;
use common_datavalues::DataValue;
use common_exception::Result;
use common_planners::EmptyPlan;
use common_planners::ExpressionAction;
use common_planners::ExpressionPlan;
use common_planners::Partitions;
use common_planners::PlanBuilder;
use common_planners::PlanNode;
use common_planners::ProjectionPlan;
use common_planners::ReadDataSourcePlan;
use common_planners::SelectPlan;
use common_planners::StageKind;
use common_planners::StagePlan;
use common_planners::Statistics;

use crate::clusters::Cluster;
use crate::clusters::ClusterRef;
use crate::clusters::Node;
use crate::configs::Config;
use crate::interpreters::plan_scheduler::PlanScheduler;
use crate::optimizers::IOptimizer;
use crate::optimizers::ScattersOptimizer;
use crate::sessions::FuseQueryContextRef;
use crate::sql::PlanParser;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_without_stage() -> Result<()> {
    let (context, cluster) = create_env().await?;
    let (local_plan, remote_plans) =
        PlanScheduler::reschedule(context.clone(), &PlanNode::Empty(EmptyPlan::create()))?;

    assert!(remote_plans.is_empty());
    assert_eq!(local_plan, PlanNode::Empty(EmptyPlan::create()));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_with_one_normal_stage() -> Result<()> {
    let (context, cluster) = create_env().await?;
    let reschedule_res = PlanScheduler::reschedule(
        context.clone(),
        &PlanNode::Stage(StagePlan {
            kind: StageKind::Normal,
            scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(1))),
            input: Arc::new(PlanNode::Empty(EmptyPlan::create()))
        })
    );

    match reschedule_res {
        Ok(_) => assert!(
            false,
            "test_scheduler_plan_with_one_normal_stage must be failure!"
        ),
        Err(error_code) => {
            assert_eq!(error_code.code(), 31);
            assert_eq!(
                error_code.message(),
                "The final stage plan must be convergent"
            );
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_with_one_expansive_stage() -> Result<()> {
    let (context, cluster) = create_env().await?;
    let reschedule_res = PlanScheduler::reschedule(
        context.clone(),
        &PlanNode::Stage(StagePlan {
            kind: StageKind::Expansive,
            scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(1))),
            input: Arc::new(PlanNode::Empty(EmptyPlan::create()))
        })
    );

    match reschedule_res {
        Ok(_) => assert!(
            false,
            "test_scheduler_plan_with_one_expansive_stage must be failure!"
        ),
        Err(error_code) => {
            assert_eq!(error_code.code(), 31);
            assert_eq!(
                error_code.message(),
                "The final stage plan must be convergent"
            );
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_with_one_convergent_stage() -> Result<()> {
    /*
     *  +------------------+
     *  |                  |
     *  |     EmptyPlan    +--------------------------+
     *  |                  |                          |
     *  +------------------+                          |
     *                                       +--------v---------+
     *                                       |                  |
     *                                       |    Remote Plan   |
     *                                       |                  |
     *  +------------------+                 +--------^---------+
     *  |                  |                          |
     *  |     EmptyPlan    +--------------------------+
     *  |                  |
     *  +------------------+
     */
    let (context, cluster) = create_env().await?;
    let (local_plan, remote_actions) = PlanScheduler::reschedule(
        context.clone(),
        &PlanNode::Stage(StagePlan {
            kind: StageKind::Convergent,
            scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(0))),
            input: Arc::new(PlanNode::Empty(EmptyPlan::create()))
        })
    )?;

    assert_eq!(remote_actions.len(), 2);
    assert_eq!(remote_actions[0].0.name, String::from("dummy_local"));
    assert_eq!(remote_actions[0].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[0].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );
    assert_eq!(
        remote_actions[0].1.plan,
        PlanNode::Empty(EmptyPlan::create())
    );

    assert_eq!(remote_actions[1].0.name, String::from("dummy"));
    assert_eq!(remote_actions[1].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[1].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );
    assert_eq!(
        remote_actions[1].1.plan,
        PlanNode::Empty(EmptyPlan::create())
    );

    match local_plan {
        PlanNode::Remote(plan) => {
            assert!(plan.fetch_name.ends_with("/dummy_local"));
            assert_eq!(plan.fetch_nodes, ["dummy_local", "dummy"]);
        }
        _ => assert!(
            false,
            "test_scheduler_plan_with_one_convergent_stage must be have Remote plan!"
        )
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_with_convergent_and_expansive_stage() -> Result<()> {
    /*
     *                  +-----------+       +-----------+
     *        +-------->|RemotePlan +------>|SelectPlan +-----------+
     *        |         +-----------+       +-----------+           |
     *        |                                                     |
     *        |                                                     v
     *   +----+------+                                        +-----------+        +-----------+
     *   | EmptyPlan |                                        |RemotePlan +------->|SelectPlan |
     *   +----+------+                                        +-----------+        +-----------+
     *        |                                                     ^
     *        |         +-----------+       +-----------+           |
     *        +-------->|RemotePlan +------>|SelectPlan +-----------+
     *                  +-----------+       +-----------+
     */
    let (context, cluster) = create_env().await?;
    let (local_plan, remote_actions) = PlanScheduler::reschedule(
        context.clone(),
        &PlanNode::Select(SelectPlan {
            input: Arc::new(PlanNode::Stage(StagePlan {
                kind: StageKind::Convergent,
                scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(0))),
                input: Arc::new(PlanNode::Select(SelectPlan {
                    input: Arc::new(PlanNode::Stage(StagePlan {
                        kind: StageKind::Expansive,
                        scatters_expr: ExpressionAction::ScalarFunction {
                            op: String::from("blockNumber"),
                            args: vec![]
                        },
                        input: Arc::new(PlanNode::Empty(EmptyPlan::create()))
                    }))
                }))
            }))
        })
    )?;

    assert_eq!(remote_actions.len(), 3);
    assert_eq!(remote_actions[0].0.name, String::from("dummy_local"));
    assert_eq!(remote_actions[0].1.scatters, vec![
        String::from("dummy_local"),
        String::from("dummy")
    ]);
    assert_eq!(
        remote_actions[0].1.scatters_action,
        ExpressionAction::ScalarFunction {
            op: String::from("blockNumber"),
            args: vec![]
        }
    );
    assert_eq!(
        remote_actions[0].1.plan,
        PlanNode::Empty(EmptyPlan::create())
    );

    assert_eq!(remote_actions[1].0.name, String::from("dummy_local"));
    assert_eq!(remote_actions[1].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[1].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );

    assert_eq!(remote_actions[2].0.name, String::from("dummy"));
    assert_eq!(remote_actions[2].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[2].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );

    // Perform the same plan in different nodes
    match (
        &remote_actions[1].1.plan,
        &remote_actions[2].1.plan,
        &local_plan
    ) {
        (PlanNode::Select(left), PlanNode::Select(right), PlanNode::Select(finalize)) => {
            match (&*left.input, &*right.input, &*finalize.input) {
                (PlanNode::Remote(left), PlanNode::Remote(right), PlanNode::Remote(finalize)) => {
                    assert!(left.fetch_name.ends_with("/dummy_local"));
                    assert!(right.fetch_name.ends_with("/dummy"));
                    assert_eq!(left.fetch_nodes, ["dummy_local"]);
                    assert_eq!(right.fetch_nodes, ["dummy_local"]);

                    assert!(finalize.fetch_name.ends_with("/dummy_local"));
                    assert_eq!(finalize.fetch_nodes, ["dummy_local", "dummy"]);
                },
                _ => assert!(false, "test_scheduler_plan_with_convergent_and_expansive_stage must be have Remote plan!"),
            }
        }
        _ => assert!(
            false,
            "test_scheduler_plan_with_convergent_and_expansive_stage must be have Select plan!"
        )
    };

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_scheduler_plan_with_convergent_and_normal_stage() -> Result<()> {
    /*
     *   +-----------+      +-----------+       +-----------+
     *   |EmptyStage +----->|RemotePlan +------>|SelectPlan +-----------+
     *   +-------+---+      +-----------+       +-----------+           |
     *           |               ^                                      |
     *           +------------+  |                                      v
     *                        |  |                                +-----------+      +-----------+
     *           +------------+--+                                |RemotePlan +----> |SelectPlan |
     *           |            |                                   +-----------+      +-----------+
     *           |            v                                         ^
     *   +-------+---+      +-----------+       +-----------+           |
     *   |EmptyStage +----->|RemotePlan +------>|SelectPlan +-----------+
     *   +-----------+      +-----------+       +-----------+
     */
    let (context, cluster) = create_env().await?;
    let (local_plan, remote_actions) = PlanScheduler::reschedule(
        context.clone(),
        &PlanNode::Select(SelectPlan {
            input: Arc::new(PlanNode::Stage(StagePlan {
                kind: StageKind::Convergent,
                scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(1))),
                input: Arc::new(PlanNode::Select(SelectPlan {
                    input: Arc::new(PlanNode::Stage(StagePlan {
                        kind: StageKind::Normal,
                        scatters_expr: ExpressionAction::Literal(DataValue::UInt64(Some(0))),
                        input: Arc::new(PlanNode::Empty(EmptyPlan::create()))
                    }))
                }))
            }))
        })
    )?;

    assert_eq!(remote_actions.len(), 4);
    assert_eq!(remote_actions[0].0.name, String::from("dummy_local"));
    assert_eq!(remote_actions[0].1.scatters, vec![
        String::from("dummy_local"),
        String::from("dummy")
    ]);
    assert_eq!(
        remote_actions[0].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );
    assert_eq!(
        remote_actions[0].1.plan,
        PlanNode::Empty(EmptyPlan::create())
    );

    assert_eq!(remote_actions[2].0.name, String::from("dummy"));
    assert_eq!(remote_actions[2].1.scatters, vec![
        String::from("dummy_local"),
        String::from("dummy")
    ]);
    assert_eq!(
        remote_actions[2].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(0)))
    );
    assert_eq!(
        remote_actions[2].1.plan,
        PlanNode::Empty(EmptyPlan::create())
    );

    assert_eq!(remote_actions[1].0.name, String::from("dummy_local"));
    assert_eq!(remote_actions[1].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[1].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(1)))
    );

    assert_eq!(remote_actions[3].0.name, String::from("dummy"));
    assert_eq!(remote_actions[3].1.scatters, vec![String::from(
        "dummy_local"
    )]);
    assert_eq!(
        remote_actions[3].1.scatters_action,
        ExpressionAction::Literal(DataValue::UInt64(Some(1)))
    );

    // Perform the same plan in different nodes
    match (
        &remote_actions[1].1.plan,
        &remote_actions[3].1.plan,
        &local_plan
    ) {
        (PlanNode::Select(left), PlanNode::Select(right), PlanNode::Select(finalize)) => {
            match (&*left.input, &*right.input, &*finalize.input) {
                (PlanNode::Remote(left), PlanNode::Remote(right), PlanNode::Remote(finalize)) => {
                    assert!(left.fetch_name.ends_with("/dummy_local"));
                    assert!(right.fetch_name.ends_with("/dummy"));
                    assert_eq!(left.fetch_nodes, ["dummy_local", "dummy"]);
                    assert_eq!(right.fetch_nodes, ["dummy_local", "dummy"]);

                    assert!(finalize.fetch_name.ends_with("/dummy_local"));
                    assert_eq!(finalize.fetch_nodes, ["dummy_local", "dummy"]);
                },
                _ => assert!(false, "test_scheduler_plan_with_convergent_and_expansive_stage must be have Remote plan!"),
            }
        }
        _ => assert!(
            false,
            "test_scheduler_plan_with_convergent_and_expansive_stage must be have Select plan!"
        )
    };

    Ok(())
}

async fn create_env() -> Result<(FuseQueryContextRef, ClusterRef)> {
    let ctx = crate::tests::try_create_context()?;
    let cluster = Cluster::create_global(Config::default())?;

    cluster
        .add_node(
            &String::from("dummy_local"),
            1,
            &String::from("localhost:9090")
        )
        .await?;
    cluster
        .add_node(&String::from("dummy"), 1, &String::from("github.com:9090"))
        .await?;
    ctx.with_cluster(cluster.clone());

    Ok((ctx, cluster))
}
