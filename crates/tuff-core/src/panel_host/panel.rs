//! The registries features fill with panel pages, APIs and metric cards.

use tuff_item::load::{LoadError, LoadErrorKind};
use tuff_item::registry::Registry;
use tuff_web_ui::panel::{Installed, MetricCard, PanelApi, PanelPage, valid_id, valid_scope};

/// The registries of pages, APIs and dashboard cards, inside
/// `Registries::panel`.
pub struct Panel {
    pages: Registry<PanelPage>,
    apis: Registry<PanelApi>,
    cards: Registry<MetricCard>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            pages: Registry::new("panel_pages"),
            apis: Registry::new("panel_apis"),
            cards: Registry::new("panel_metric_cards"),
        }
    }
}

fn invalid(feature: &'static str, message: String) -> LoadError {
    LoadError {
        feature,
        kind: LoadErrorKind::Other(message),
    }
}

impl Panel {
    pub fn register_page(
        &mut self,
        feature: &'static str,
        page: PanelPage,
    ) -> Result<(), LoadError> {
        page.validate().map_err(|m| invalid(feature, m))?;
        let id = page.id.clone();
        self.pages.register(feature, id, page)
    }

    pub fn register_api(&mut self, feature: &'static str, api: PanelApi) -> Result<(), LoadError> {
        valid_id(&api.id).map_err(|m| invalid(feature, m))?;
        valid_scope(api.scope).map_err(|m| invalid(feature, m))?;
        let id = api.id.clone();
        self.apis.register(feature, id, api)
    }

    pub fn register_card(
        &mut self,
        feature: &'static str,
        card: MetricCard,
    ) -> Result<(), LoadError> {
        card.validate().map_err(|m| invalid(feature, m))?;
        let series = card.series.clone();
        self.cards.register(feature, series, card)
    }

    pub fn freeze(&mut self) {
        self.pages.freeze();
        self.apis.freeze();
        self.cards.freeze();
    }

    /// The registered pages and APIs, for serving.
    pub fn installed(&self) -> Installed {
        Installed {
            pages: self
                .pages
                .iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect(),
            apis: self
                .apis
                .iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect(),
            cards: self
                .cards
                .iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tuff_http::Response;
    use tuff_storage::admin::PermissionLevel;
    use tuff_web_ui::auth::Principal;
    use tuff_web_ui::auth::{AuthContext, Scope};
    use tuff_web_ui::panel::*;

    fn page(id: &str) -> PanelPage {
        PanelPage::new(
            id,
            "Title",
            Scope::Plugin("test.read"),
            Asset::module("main.js", "export function mount() {}"),
        )
    }

    fn ctx(level: PermissionLevel) -> AuthContext {
        AuthContext {
            principal: Principal::Operator {
                uuid: 1,
                name: "op".into(),
            },
            level,
            session: "s".into(),
            csrf: String::new(),
            permissions: std::sync::Arc::new(crate::permission::View::default()),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn duplicate_page_ids_name_both_features() {
        let mut panel = Panel::default();
        panel.register_page("first", page("gold")).unwrap();
        let err = panel.register_page("second", page("gold")).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("first") && text.contains("second"), "{text}");
        let api = PanelApi::new(
            "gold",
            Scope::Plugin("test.read"),
            api_handler(|_| async { Response::status(hyper::StatusCode::OK) }),
        );
        panel.register_api("first", api.clone()).unwrap();
        let err = panel.register_api("second", api).unwrap_err().to_string();
        assert!(err.contains("first") && err.contains("second"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn pages_are_validated() {
        let mut panel = Panel::default();
        assert!(panel.register_page("p", page("Bad Id")).is_err());
        assert!(panel.register_page("p", page("../x")).is_err());
        let mut no_module = page("a");
        no_module.module = "other.js".into();
        assert!(panel.register_page("p", no_module).is_err());
        let not_js = PanelPage::new(
            "b",
            "B",
            Scope::Plugin("test.read"),
            Asset::new("main.js", "text/html", Bytes::new()),
        );
        assert!(panel.register_page("p", not_js).is_err());
        for path in ["../x.js", "/x.js", "a//b.js", "a/./b.js", "a b.js"] {
            let bad = page("c").asset(Asset::new(path, "text/css", Bytes::new()));
            assert!(panel.register_page("p", bad).is_err(), "{path}");
        }
        let mut bad_style = page("f");
        bad_style.styles.push("main.js".into());
        assert!(panel.register_page("p", bad_style).is_err());
        let twice = page("d").asset(Asset::module("main.js", ""));
        assert!(panel.register_page("p", twice).is_err());
        let mut unscoped = page("e");
        unscoped.scope = Scope::Plugin("read");
        assert!(panel.register_page("p", unscoped).is_err());
        assert!(
            panel
                .register_page(
                    "p",
                    page("ok").style(Asset::new("css/a.css", "text/css", Bytes::new()))
                )
                .is_ok()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn metric_cards_are_validated_keyed_and_scoped() {
        let mut panel = Panel::default();
        panel
            .register_card("first", MetricCard::new("map.tiles", "Tiles"))
            .unwrap();
        let err = panel
            .register_card("second", MetricCard::new("map.tiles", "Again"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("first") && err.contains("second"), "{err}");
        for bad in ["", "Map.Tiles", "a b", &"x".repeat(129)] {
            assert!(
                panel.register_card("p", MetricCard::new(bad, "T")).is_err(),
                "{bad:?}"
            );
        }
        assert!(
            panel
                .register_card("p", MetricCard::new("ok", " "))
                .is_err()
        );
        let secret = MetricCard::new("map.secret", "Secret")
            .unit(Unit::Bytes)
            .scope(Scope::Plugin("map.admin"));
        panel.register_card("p", secret).unwrap();
        panel.freeze();
        let installed = panel.installed();
        let series = |level| -> Vec<String> {
            installed
                .cards_for(&ctx(level))
                .iter()
                .map(|c| c.series.clone())
                .collect()
        };
        assert_eq!(series(PermissionLevel::Owners), ["map.secret", "map.tiles"]);
        assert!(series(PermissionLevel::Admins).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn listing_and_topics_follow_scopes() {
        let mut panel = Panel::default();
        panel.register_page("p", page("gold").section("B")).unwrap();
        let mut logs = page("logs").section("A");
        logs.scope = Scope::LogsRead;
        panel.register_page("p", logs).unwrap();
        panel.freeze();
        let installed = panel.installed();
        let ids = |level| -> Vec<String> {
            installed
                .visible(&ctx(level))
                .iter()
                .map(|p| p.id.clone())
                .collect()
        };
        assert_eq!(ids(PermissionLevel::Owners), ["logs", "gold"]);
        assert_eq!(ids(PermissionLevel::Gamemasters), ["logs"]);
        let owner = ctx(PermissionLevel::Owners);
        let helper = ctx(PermissionLevel::Gamemasters);
        assert!(installed.topic_allowed(&owner, "plugin.gold.count"));
        assert!(!installed.topic_allowed(&helper, "plugin.gold.count"));
        assert!(!installed.topic_allowed(&owner, "plugin.gold"));
        assert!(!installed.topic_allowed(&owner, "plugin.none.count"));
        assert!(installed.topic_allowed(&helper, "plugin.logs.x"));
    }
}
