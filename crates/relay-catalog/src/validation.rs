use std::{collections::BTreeSet, fmt};

use crate::{Lab, Model, Provider, ProviderModel, Slug};

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Issue {
    DuplicateLab {
        id: Slug,
    },
    DuplicateProvider {
        id: Slug,
    },
    DuplicateModel {
        id: Slug,
    },
    DuplicateProviderModel {
        provider: Slug,
        upstream_slug: String,
    },
    UnknownLab {
        model: Slug,
        lab: Slug,
    },
    UnknownProvider {
        provider: Slug,
        upstream_slug: String,
    },
    UnknownModel {
        provider: Slug,
        upstream_slug: String,
        model: Slug,
    },
}

impl fmt::Display for Issue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateLab { id } => write!(formatter, "duplicate lab {id}"),
            Self::DuplicateProvider { id } => write!(formatter, "duplicate provider {id}"),
            Self::DuplicateModel { id } => write!(formatter, "duplicate model {id}"),
            Self::DuplicateProviderModel {
                provider,
                upstream_slug,
            } => write!(
                formatter,
                "duplicate provider model ({provider}, {upstream_slug})"
            ),
            Self::UnknownLab { model, lab } => {
                write!(formatter, "model {model} references unknown lab {lab}")
            }
            Self::UnknownProvider {
                provider,
                upstream_slug,
            } => write!(
                formatter,
                "provider model ({provider}, {upstream_slug}) references unknown provider"
            ),
            Self::UnknownModel {
                provider,
                upstream_slug,
                model,
            } => write!(
                formatter,
                "provider model ({provider}, {upstream_slug}) references unknown model {model}"
            ),
        }
    }
}

/// Reports all issues in list and input order, with duplicates before references per record.
pub fn validate(
    labs: &[Lab],
    providers: &[Provider],
    models: &[Model],
    provider_models: &[ProviderModel],
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut lab_ids = BTreeSet::new();
    for lab in labs {
        if !lab_ids.insert(lab.id.as_str()) {
            issues.push(Issue::DuplicateLab { id: lab.id.clone() });
        }
    }

    let mut provider_ids = BTreeSet::new();
    for provider in providers {
        if !provider_ids.insert(provider.id.as_str()) {
            issues.push(Issue::DuplicateProvider {
                id: provider.id.clone(),
            });
        }
    }

    let mut model_ids = BTreeSet::new();
    for model in models {
        if !model_ids.insert(model.id.as_str()) {
            issues.push(Issue::DuplicateModel {
                id: model.id.clone(),
            });
        }
        if !lab_ids.contains(model.lab.as_str()) {
            issues.push(Issue::UnknownLab {
                model: model.id.clone(),
                lab: model.lab.clone(),
            });
        }
    }

    let mut provider_model_keys = BTreeSet::new();
    for provider_model in provider_models {
        if !provider_model_keys.insert((
            provider_model.provider.as_str(),
            provider_model.upstream_slug.as_str(),
        )) {
            issues.push(Issue::DuplicateProviderModel {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
            });
        }
        if !provider_ids.contains(provider_model.provider.as_str()) {
            issues.push(Issue::UnknownProvider {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
            });
        }
        if !model_ids.contains(provider_model.model.as_str()) {
            issues.push(Issue::UnknownModel {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
                model: provider_model.model.clone(),
            });
        }
    }
    issues
}
