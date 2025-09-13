// Copyright 2025 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::ops::Range;

use indexmap::IndexMap;

use crate::backend::CommitId;
use crate::fileset::FilesetExpression;
use crate::revset::ResolvedExpression;
use crate::revset::ResolvedPredicateExpression;
use crate::revset::RevsetFilterPredicate;
use crate::str_util::StringPattern;
use crate::time_util::DatePattern;

#[derive(Default, Debug)]
pub(super) struct RevsetOptimizerState<'a> {
    nodes: IndexMap<OptimizedRevsetExpression<'a>, RevsetUsage>,
}

impl<'a> RevsetOptimizerState<'a> {
    pub(super) fn get(
        &self,
        revset_ref: RevsetRef,
    ) -> (&OptimizedRevsetExpression<'a>, RevsetUsage) {
        let (node, &usage) = self
            .nodes
            .get_index(revset_ref.0)
            .expect("revset index should be in bounds");
        (node, usage)
    }

    pub(super) fn insert(&mut self, revset: &'a ResolvedExpression) -> RevsetRef {
        let optimized = match revset {
            ResolvedExpression::Commits(commit_ids) => {
                OptimizedRevsetExpression::Commits(&commit_ids)
            }
            ResolvedExpression::Ancestors {
                heads,
                generation,
                parents_range,
            } => OptimizedRevsetExpression::Ancestors {
                heads: self.insert(heads),
                generation: generation.clone(),
                parents_range: parents_range.clone(),
            },
            ResolvedExpression::Range {
                roots,
                heads,
                generation,
                parents_range,
            } => OptimizedRevsetExpression::Range {
                roots: self.insert(roots),
                heads: self.insert(heads),
                generation: generation.clone(),
                parents_range: parents_range.clone(),
            },
            ResolvedExpression::DagRange {
                roots,
                heads,
                generation_from_roots,
            } => OptimizedRevsetExpression::DagRange {
                roots: self.insert(roots),
                heads: self.insert(heads),
                generation_from_roots: generation_from_roots.clone(),
            },
            ResolvedExpression::Reachable { sources, domain } => {
                OptimizedRevsetExpression::Reachable {
                    sources: self.insert(sources),
                    domain: self.insert(domain),
                }
            }
            ResolvedExpression::Heads(candidates) => {
                OptimizedRevsetExpression::Heads(self.insert(candidates))
            }
            ResolvedExpression::HeadsRange {
                roots,
                heads,
                parents_range,
                filter,
            } => OptimizedRevsetExpression::HeadsRange {
                roots: self.insert(roots),
                heads: self.insert(heads),
                parents_range: parents_range.clone(),
                filter: filter.as_ref().map(|f| self.optimize_filter(f)),
            },
            ResolvedExpression::Roots(candidates) => {
                OptimizedRevsetExpression::Roots(self.insert(candidates))
            }
            ResolvedExpression::ForkPoint(expression) => {
                OptimizedRevsetExpression::ForkPoint(self.insert(expression))
            }
            ResolvedExpression::Bisect(expression) => {
                OptimizedRevsetExpression::Bisect(self.insert(expression))
            }
            ResolvedExpression::Latest { candidates, count } => OptimizedRevsetExpression::Latest {
                candidates: self.insert(candidates),
                count: *count,
            },
            ResolvedExpression::Coalesce(left, right) => {
                OptimizedRevsetExpression::Coalesce(self.insert(left), self.insert(right))
            }
            ResolvedExpression::Union(left, right) => {
                OptimizedRevsetExpression::Union(self.insert(left), self.insert(right))
            }
            ResolvedExpression::FilterWithin {
                candidates,
                predicate,
            } => OptimizedRevsetExpression::FilterWithin {
                candidates: self.insert(candidates),
                predicate: self.optimize_filter(predicate),
            },
            ResolvedExpression::Intersection(left, right) => {
                OptimizedRevsetExpression::Intersection(self.insert(left), self.insert(right))
            }
            ResolvedExpression::Difference(left, right) => {
                OptimizedRevsetExpression::Difference(self.insert(left), self.insert(right))
            }
        };
        let entry = self.nodes.entry(optimized);
        let index = entry.index();
        entry.and_modify(|e| *e = RevsetUsage::Many).or_default();
        RevsetRef(index)
    }

    fn optimize_filter(
        &mut self,
        predicate: &'a ResolvedPredicateExpression,
    ) -> OptimizedPredicateExpression<'a> {
        match predicate {
            ResolvedPredicateExpression::Filter(filter) => {
                let hashable = match filter {
                    RevsetFilterPredicate::ParentCount(n) => {
                        Some(HashableFilterPredicate::ParentCount(n))
                    }
                    RevsetFilterPredicate::Description(StringPattern::Exact(s)) => {
                        Some(HashableFilterPredicate::DescriptionExact(s))
                    }
                    RevsetFilterPredicate::Subject(StringPattern::Exact(s)) => {
                        Some(HashableFilterPredicate::SubjectExact(s))
                    }
                    RevsetFilterPredicate::AuthorEmail(StringPattern::ExactI(s)) => {
                        Some(HashableFilterPredicate::AuthorEmailExactI(s))
                    }
                    RevsetFilterPredicate::AuthorDate(date) => {
                        Some(HashableFilterPredicate::AuthorDate(date))
                    }
                    RevsetFilterPredicate::CommitterDate(date) => {
                        Some(HashableFilterPredicate::CommitterDate(date))
                    }
                    RevsetFilterPredicate::File(fileset) => {
                        Some(HashableFilterPredicate::File(fileset))
                    }
                    RevsetFilterPredicate::HasConflict => {
                        Some(HashableFilterPredicate::HasConflict)
                    }
                    RevsetFilterPredicate::Signed => Some(HashableFilterPredicate::Signed),
                    // These filters we will compare by reference equality
                    RevsetFilterPredicate::Description(_)
                    | RevsetFilterPredicate::Subject(_)
                    | RevsetFilterPredicate::AuthorName(_)
                    | RevsetFilterPredicate::AuthorEmail(_)
                    | RevsetFilterPredicate::CommitterName(_)
                    | RevsetFilterPredicate::CommitterEmail(_)
                    | RevsetFilterPredicate::DiffContains { .. }
                    | RevsetFilterPredicate::Extension(_) => None,
                };
                OptimizedPredicateExpression::Filter(OptimizedFilterPredicate {
                    filter,
                    hashable: hashable,
                })
            }
            ResolvedPredicateExpression::Set(set) => {
                OptimizedPredicateExpression::Set(self.insert(set))
            }
            ResolvedPredicateExpression::NotIn(predicate) => {
                OptimizedPredicateExpression::NotIn(Box::new(self.optimize_filter(predicate)))
            }
            ResolvedPredicateExpression::Union(left, right) => OptimizedPredicateExpression::Union(
                Box::new(self.optimize_filter(left)),
                Box::new(self.optimize_filter(right)),
            ),
            ResolvedPredicateExpression::Intersection(left, right) => {
                OptimizedPredicateExpression::Intersection(
                    Box::new(self.optimize_filter(left)),
                    Box::new(self.optimize_filter(right)),
                )
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub(super) enum RevsetUsage {
    #[default]
    Once,
    Many,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct RevsetRef(usize);

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) enum OptimizedRevsetExpression<'a> {
    Commits(&'a [CommitId]),
    Ancestors {
        heads: RevsetRef,
        generation: Range<u64>,
        parents_range: Range<u32>,
    },
    Range {
        roots: RevsetRef,
        heads: RevsetRef,
        generation: Range<u64>,
        parents_range: Range<u32>,
    },
    DagRange {
        roots: RevsetRef,
        heads: RevsetRef,
        generation_from_roots: Range<u64>,
    },
    Reachable {
        sources: RevsetRef,
        domain: RevsetRef,
    },
    Heads(RevsetRef),
    HeadsRange {
        roots: RevsetRef,
        heads: RevsetRef,
        parents_range: Range<u32>,
        filter: Option<OptimizedPredicateExpression<'a>>,
    },
    Roots(RevsetRef),
    ForkPoint(RevsetRef),
    Bisect(RevsetRef),
    Latest {
        candidates: RevsetRef,
        count: usize,
    },
    Coalesce(RevsetRef, RevsetRef),
    Union(RevsetRef, RevsetRef),
    FilterWithin {
        candidates: RevsetRef,
        predicate: OptimizedPredicateExpression<'a>,
    },
    Intersection(RevsetRef, RevsetRef),
    Difference(RevsetRef, RevsetRef),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) enum OptimizedPredicateExpression<'a> {
    Filter(OptimizedFilterPredicate<'a>),
    Set(RevsetRef),
    NotIn(Box<Self>),
    Union(Box<Self>, Box<Self>),
    Intersection(Box<Self>, Box<Self>),
}

#[derive(Clone, Debug)]
pub(super) struct OptimizedFilterPredicate<'a> {
    pub(super) filter: &'a RevsetFilterPredicate,
    hashable: Option<HashableFilterPredicate<'a>>,
}

impl PartialEq for OptimizedFilterPredicate<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (&self.hashable, &other.hashable) {
            (Some(a), Some(b)) => a == b,
            (None, None) => std::ptr::eq(self.filter, other.filter),
            _ => false,
        }
    }
}

impl Eq for OptimizedFilterPredicate<'_> {}

impl std::hash::Hash for OptimizedFilterPredicate<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        if let Some(hashable) = &self.hashable {
            hashable.hash(state);
        } else {
            std::ptr::hash(self.filter, state);
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) enum HashableFilterPredicate<'a> {
    ParentCount(&'a Range<u32>),
    DescriptionExact(&'a str),
    SubjectExact(&'a str),
    // Useful for "mine()"
    AuthorEmailExactI(&'a str),
    AuthorDate(&'a DatePattern),
    CommitterDate(&'a DatePattern),
    File(&'a FilesetExpression),
    HasConflict,
    Signed,
}
