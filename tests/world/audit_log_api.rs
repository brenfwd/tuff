//! The audit log's plugin-facing API and per-group retention.

use super::*;
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::{Group, Kind, Record, Who, now_ms};
use tuff_feature_builtin_audit_log::service::group_retention_key;
use tuff_feature_builtin_audit_log::store::Order;
use tuff_feature_builtin_audit_log::{AuditLog, AuditLogFeature};

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

async fn kinds(log: &AuditLog) -> Vec<Kind> {
    log.sync().await.unwrap();
    log.lookup(Filter::default(), Order::OldestFirst, 100, 0)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|row| row.kind)
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_feature_logs_and_looks_up_a_custom_action() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let log = world.shared.services.get::<AuditLog>().unwrap();
    let who = Who::cause("shop");
    let overworld = world.shared.dimensions.iter().next().unwrap().id;
    assert!(
        log.log_custom(who.clone(), "bad name", overworld, [0; 3], None)
            .is_err()
    );
    log.log_custom(
        who,
        "shop:sale",
        overworld,
        [1, 2, 3],
        Some("2 diamonds".into()),
    )
    .unwrap();
    log.sync().await.unwrap();
    let rows = log
        .lookup(
            Filter {
                kinds: vec![Kind::Custom],
                include: vec!["shop:sale".to_owned()],
                ..Filter::default()
            },
            Order::OldestFirst,
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text.as_deref(), Some("2 diamonds"));
    assert_eq!(rows[0].pos, [1, 2, 3]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_group_with_its_own_retention_is_purged_by_it() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let log = world.shared.services.get::<AuditLog>().unwrap();
    let overworld = world.shared.dimensions.iter().next().unwrap().id;
    let old = now_ms() - 10 * 86_400_000;
    for kind in [Kind::Chat, Kind::Command] {
        log.log(Record::new(old, Who::cause("x"), kind, overworld, [0; 3]));
    }
    assert_eq!(kinds(log).await.len(), 2);
    world
        .shared
        .config
        .set_text(&group_retention_key(Group::Chat), "1d")
        .unwrap();
    log.purge_by_retention(&world.shared.config);
    log.sync().await.unwrap();
    for _ in 0..100 {
        if kinds(log).await.len() == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(kinds(log).await, [Kind::Command]);
}
