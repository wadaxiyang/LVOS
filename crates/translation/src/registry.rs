use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use crate::{
    DEFAULT_FALLBACK_PROVIDER, DEFAULT_PRIMARY_PROVIDER, ProviderId, TranslationError,
    TranslationProvider, TranslationRequest, TranslationResult,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouterSettings {
    pub primary: ProviderId,
    pub fallback: Option<ProviderId>,
}

impl Default for RouterSettings {
    fn default() -> Self {
        Self {
            primary: ProviderId::new(DEFAULT_PRIMARY_PROVIDER),
            fallback: Some(ProviderId::new(DEFAULT_FALLBACK_PROVIDER)),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Arc<dyn TranslationProvider>>,
}

impl ProviderRegistry {
    pub fn register(&mut self, provider: Arc<dyn TranslationProvider>) {
        self.providers.insert(provider.id(), provider);
    }

    #[must_use]
    pub fn contains(&self, id: &ProviderId) -> bool {
        self.providers.contains_key(id)
    }

    #[must_use]
    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn TranslationProvider>> {
        self.providers.get(id).cloned()
    }

    /// Rejects settings whose selected providers are absent or duplicate each other.
    ///
    /// # Errors
    /// Returns an error when a selected Provider is missing or both selections are identical.
    pub fn validate(&self, settings: &RouterSettings) -> Result<(), SettingsError> {
        if !self.contains(&settings.primary) {
            return Err(SettingsError::ProviderNotConfigured(
                settings.primary.clone(),
            ));
        }
        if let Some(fallback) = &settings.fallback {
            if fallback == &settings.primary {
                return Err(SettingsError::DuplicateProvider);
            }
            if !self.contains(fallback) {
                return Err(SettingsError::ProviderNotConfigured(fallback.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TranslationRouter {
    primary: Arc<dyn TranslationProvider>,
    fallback: Option<Arc<dyn TranslationProvider>>,
}

impl TranslationRouter {
    /// Builds a router only after selected Provider configuration is complete.
    ///
    /// # Errors
    /// Returns an error when selected Provider configuration is invalid.
    pub fn new(
        registry: &ProviderRegistry,
        settings: &RouterSettings,
    ) -> Result<Self, SettingsError> {
        registry.validate(settings)?;
        let primary = registry
            .get(&settings.primary)
            .ok_or_else(|| SettingsError::ProviderNotConfigured(settings.primary.clone()))?;
        let fallback = settings
            .fallback
            .as_ref()
            .map(|id| {
                registry
                    .get(id)
                    .ok_or_else(|| SettingsError::ProviderNotConfigured(id.clone()))
            })
            .transpose()?;
        Ok(Self { primary, fallback })
    }

    /// Runs Primary first and invokes Fallback only for an explicitly transient failure.
    ///
    /// # Errors
    /// Returns the non-fallback error, or the Fallback result when Fallback was attempted.
    pub async fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<TranslationResult, TranslationError> {
        match self.primary.translate(request).await {
            Ok(result) => Ok(result),
            Err(error) if error.permits_fallback() => {
                if let Some(fallback) = &self.fallback {
                    fallback.translate(request).await
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsError {
    ProviderNotConfigured(ProviderId),
    DuplicateProvider,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderNotConfigured(provider) => {
                write!(formatter, "selected Provider {provider} is not configured")
            }
            Self::DuplicateProvider => {
                formatter.write_str("Primary and Fallback Provider must be different")
            }
        }
    }
}

impl Error for SettingsError {}
