// Copyright 2021 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;

use databend_common_exception::ErrorCode;
use databend_common_exception::Result;
use databend_common_expression::FunctionKind;
use databend_common_functions::BUILTIN_FUNCTIONS;

use crate::binder::select::SelectList;
use crate::binder::ColumnBindingBuilder;
use crate::format_scalar;
use crate::optimizer::SExpr;
use crate::plans::walk_expr_mut;
use crate::plans::BoundColumnRef;
use crate::plans::ProjectSet;
use crate::plans::ScalarItem;
use crate::plans::VisitorMut;
use crate::BindContext;
use crate::Binder;
use crate::MetadataRef;
use crate::ScalarExpr;
use crate::Visibility;

#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct SetReturningInfo {
    /// Set-returning functions.
    pub srfs: Vec<ScalarItem>,
    /// Mapping: (Set-returning function display name) -> (index of Set-returning function in `srfs`)
    /// This is used to find a Set-returning function in current context.
    pub srfs_map: HashMap<String, usize>,
    /// The lazy index of Set-returning functions in `srfs`.
    /// Those set-returning function's argument contains aggregate functions or group by items.
    /// Build a lazy `ProjectSet` plan after the `Aggregate` plan.
    pub lazy_srf_set: HashSet<usize>,
}

/// Rewrite Set-returning functions as a BoundColumnRef.
pub(crate) struct SetReturningRewriter<'a> {
    pub(crate) bind_context: &'a mut BindContext,
    // Whether only rewrite the argument of aggregate function.
    only_agg: bool,
    // Whether current function is the argument of aggregate function.
    is_agg_arg: bool,
}

impl<'a> SetReturningRewriter<'a> {
    pub(crate) fn new(bind_context: &'a mut BindContext, only_agg: bool) -> Self {
        Self {
            bind_context,
            only_agg,
            is_agg_arg: false,
        }
    }
}

impl<'a> VisitorMut<'a> for SetReturningRewriter<'a> {
    fn visit(&mut self, expr: &'a mut ScalarExpr) -> Result<()> {
        match expr {
            ScalarExpr::FunctionCall(func) => {
                if (self.is_agg_arg || !self.only_agg)
                    && BUILTIN_FUNCTIONS
                        .get_property(&func.func_name)
                        .map(|property| property.kind == FunctionKind::SRF)
                        .unwrap_or(false)
                {
                    let srf_display_name = format_scalar(expr);
                    if let Some(index) = self.bind_context.srf_info.srfs_map.get(&srf_display_name)
                    {
                        let srf_item = &self.bind_context.srf_info.srfs[*index];

                        let column_binding = ColumnBindingBuilder::new(
                            srf_display_name,
                            srf_item.index,
                            Box::new(srf_item.scalar.data_type()?),
                            Visibility::InVisible,
                        )
                        .build();
                        *expr = BoundColumnRef {
                            span: None,
                            column: column_binding,
                        }
                        .into();

                        return Ok(());
                    }
                    return Err(ErrorCode::Internal("Invalid Set-returning function"));
                }
            }
            ScalarExpr::AggregateFunction(_) => {
                self.is_agg_arg = true;
            }
            _ => {}
        }
        walk_expr_mut(self, expr)?;

        self.is_agg_arg = false;
        Ok(())
    }
}

/// Analyze Set-returning functions and create derived columns.
struct SetReturningAnalyzer<'a> {
    bind_context: &'a mut BindContext,
    metadata: MetadataRef,
}

impl<'a> SetReturningAnalyzer<'a> {
    fn new(bind_context: &'a mut BindContext, metadata: MetadataRef) -> Self {
        Self {
            bind_context,
            metadata,
        }
    }
}

impl<'a> VisitorMut<'a> for SetReturningAnalyzer<'a> {
    fn visit(&mut self, expr: &'a mut ScalarExpr) -> Result<()> {
        if let ScalarExpr::FunctionCall(func) = expr {
            if BUILTIN_FUNCTIONS
                .get_property(&func.func_name)
                .map(|property| property.kind == FunctionKind::SRF)
                .unwrap_or(false)
            {
                let srf_display_name = format_scalar(expr);
                let index = self.metadata.write().add_derived_column(
                    srf_display_name.clone(),
                    expr.data_type()?,
                    Some(expr.clone()),
                );

                // Add the srf to bind context, build ProjectSet plan later.
                self.bind_context.srf_info.srfs.push(ScalarItem {
                    index,
                    scalar: expr.clone(),
                });
                self.bind_context
                    .srf_info
                    .srfs_map
                    .insert(srf_display_name, self.bind_context.srf_info.srfs.len() - 1);
                return Ok(());
            }
        }

        walk_expr_mut(self, expr)
    }
}

/// Check whether the argument of Set-returning functions contains aggregation function or group item.
/// If true, we need to lazy build `ProjectSet` plan
struct SetReturningChecker<'a> {
    bind_context: &'a mut BindContext,
    has_aggregate_argument: bool,
}

impl<'a> SetReturningChecker<'a> {
    fn new(bind_context: &'a mut BindContext) -> Self {
        Self {
            bind_context,
            has_aggregate_argument: false,
        }
    }
}

impl<'a> VisitorMut<'a> for SetReturningChecker<'a> {
    fn visit(&mut self, expr: &'a mut ScalarExpr) -> Result<()> {
        if self
            .bind_context
            .aggregate_info
            .group_items_map
            .contains_key(expr)
        {
            self.has_aggregate_argument = true;
        }

        if let ScalarExpr::AggregateFunction(agg_func) = expr {
            self.has_aggregate_argument = true;
            if let Some(index) = self
                .bind_context
                .aggregate_info
                .aggregate_functions_map
                .get(&agg_func.display_name)
            {
                let agg_item = &self.bind_context.aggregate_info.aggregate_functions[*index];
                let column_binding = ColumnBindingBuilder::new(
                    agg_func.display_name.clone(),
                    agg_item.index,
                    Box::new(agg_item.scalar.data_type()?),
                    Visibility::InVisible,
                )
                .build();

                let column_ref: ScalarExpr = BoundColumnRef {
                    span: expr.span(),
                    column: column_binding.clone(),
                }
                .into();
                *expr = column_ref;
            }
            return Ok(());
        }

        walk_expr_mut(self, expr)
    }
}

impl Binder {
    /// Analyze project sets in select clause.
    /// See [`SetReturningAnalyzer`] for more details.
    pub(crate) fn analyze_project_set_select(
        &mut self,
        bind_context: &mut BindContext,
        select_list: &mut SelectList,
    ) -> Result<()> {
        let mut analyzer = SetReturningAnalyzer::new(bind_context, self.metadata.clone());
        for item in select_list.items.iter_mut() {
            analyzer.visit(&mut item.scalar)?;
        }

        Ok(())
    }

    pub(crate) fn check_project_set_select(
        &mut self,
        bind_context: &mut BindContext,
    ) -> Result<()> {
        let mut srf_info = mem::take(&mut bind_context.srf_info);
        let mut checker = SetReturningChecker::new(bind_context);
        for srf_item in srf_info.srfs.iter_mut() {
            let srf_display_name = format_scalar(&srf_item.scalar);
            checker.has_aggregate_argument = false;
            checker.visit(&mut srf_item.scalar)?;

            // If the argument contains aggregation function or group item.
            // add the srf index to lazy set.
            if checker.has_aggregate_argument {
                // srf_display_names.push(srf_display_name);
                // if let Some(index) = bind_context.srf_info.srfs_map.get(&srf_display_name) {
                //    bind_context.srf_info.lazy_srf_set.insert(*index);
                if let Some(index) = srf_info.srfs_map.get(&srf_display_name) {
                    srf_info.lazy_srf_set.insert(*index);
                }
            }
        }
        bind_context.srf_info = srf_info;

        Ok(())
    }

    pub(crate) fn bind_project_set(
        &mut self,
        bind_context: &mut BindContext,
        child: SExpr,
        is_lazy: bool,
    ) -> Result<SExpr> {
        let srf_len = if is_lazy {
            bind_context.srf_info.lazy_srf_set.len()
        } else {
            bind_context.srf_info.srfs.len() - bind_context.srf_info.lazy_srf_set.len()
        };
        if srf_len == 0 {
            return Ok(child);
        }

        // Build a ProjectSet Plan.
        let mut srfs = Vec::with_capacity(srf_len);
        for (i, srf) in bind_context.srf_info.srfs.iter().enumerate() {
            let is_lazy_srf = bind_context.srf_info.lazy_srf_set.contains(&i);
            if (is_lazy && is_lazy_srf) || (!is_lazy && !is_lazy_srf) {
                srfs.push(srf.clone());
            }
        }

        let project_set = ProjectSet { srfs };
        let new_expr = SExpr::create_unary(Arc::new(project_set.into()), Arc::new(child));

        Ok(new_expr)
    }
}
