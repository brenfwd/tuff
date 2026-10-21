use crate::common;

use common::{MockSessionServer, start_server};
use serde_json::Value;

async fn run(server: &common::TestServer, line: &str) -> Vec<Value> {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|line| line.to_value())
        .collect()
}

fn key(lines: &[Value]) -> &str {
    lines[0]["translate"].as_str().unwrap_or_default()
}

fn arg(lines: &[Value], index: usize) -> String {
    let node = &lines[0]["with"][index];
    match node["extra"].as_array() {
        Some(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        None => node["text"].as_str().unwrap_or_default().to_string(),
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn data_reads_and_edits_command_storage_through_paths() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let missing = run(&server, "data get storage test:a x").await;
    assert_eq!(key(&missing), "arguments.nbtpath.nothing_found");

    let merged = run(
        &server,
        "data merge storage test:a {x:1,l:[1,2],o:{k:\"v\"}}",
    )
    .await;
    assert_eq!(key(&merged), "commands.data.storage.modified");
    assert_eq!(arg(&merged, 0), "test:a");
    let again = run(&server, "data merge storage test:a {x:1}").await;
    assert_eq!(key(&again), "commands.data.merge.failed");

    let whole = run(&server, "data get storage test:a").await;
    assert_eq!(key(&whole), "commands.data.storage.query");
    assert_eq!(arg(&whole, 1), "{x: 1, l: [1, 2], o: {k: \"v\"}}");
    let one = run(&server, "data get storage test:a l[1]").await;
    assert_eq!(arg(&one, 1), "2");
    let scaled = run(&server, "data get storage test:a x 2.5").await;
    assert_eq!(key(&scaled), "commands.data.storage.get");
    assert_eq!(arg(&scaled, 0), "x");
    assert_eq!(arg(&scaled, 2), "2.50");
    assert_eq!(arg(&scaled, 3), "2");
    let not_number = run(&server, "data get storage test:a o.k 2").await;
    assert_eq!(key(&not_number), "commands.data.get.invalid");
    let many = run(&server, "data get storage test:a l[]").await;
    assert_eq!(key(&many), "commands.data.get.multiple");

    let appended = run(&server, "data modify storage test:a l append value 3").await;
    assert_eq!(key(&appended), "commands.data.storage.modified");
    run(&server, "data modify storage test:a l prepend value 0").await;
    run(&server, "data modify storage test:a l insert 2 value 9").await;
    let list = run(&server, "data get storage test:a l").await;
    assert_eq!(arg(&list, 1), "[0, 1, 9, 2, 3]");
    run(&server, "data modify storage test:a o.n set value 5b").await;
    run(&server, "data modify storage test:a o merge value {m:2}").await;
    let object = run(&server, "data get storage test:a o").await;
    assert_eq!(arg(&object, 1), "{k: \"v\", n: 5b, m: 2}");
    let unchanged = run(&server, "data modify storage test:a o.n set value 5b").await;
    assert_eq!(key(&unchanged), "commands.data.merge.failed");

    run(
        &server,
        "data modify storage test:b copy set from storage test:a o.k",
    )
    .await;
    run(
        &server,
        "data modify storage test:b text set string storage test:a o.k 0 1",
    )
    .await;
    let copied = run(&server, "data get storage test:b").await;
    assert_eq!(arg(&copied, 1), "{copy: \"v\", text: \"v\"}");
    let bad_range = run(
        &server,
        "data modify storage test:b text set string storage test:a o.k 0 4",
    )
    .await;
    assert_eq!(key(&bad_range), "commands.data.modify.invalid_substring");
    let not_text = run(
        &server,
        "data modify storage test:b text set string storage test:a l",
    )
    .await;
    assert_eq!(key(&not_text), "commands.data.modify.expected_value");

    let removed = run(&server, "data remove storage test:a l[0]").await;
    assert_eq!(key(&removed), "commands.data.storage.modified");
    let gone = run(&server, "data remove storage test:a nothing").await;
    assert_eq!(key(&gone), "commands.data.merge.failed");
    let list = run(&server, "data get storage test:a l").await;
    assert_eq!(arg(&list, 1), "[1, 9, 2, 3]");

    let tested = run(&server, "execute if data storage test:a l[]").await;
    assert_eq!(key(&tested), "commands.execute.conditional.pass_count");
    assert_eq!(arg(&tested, 0), "4");
    let absent = run(&server, "execute if data storage test:a nothing").await;
    assert_eq!(key(&absent), "commands.execute.conditional.fail");
    let unless = run(&server, "execute unless data storage test:a nothing").await;
    assert_eq!(key(&unless), "commands.execute.conditional.pass");
    let blocked = run(&server, "execute unless data storage test:a l[]").await;
    assert_eq!(key(&blocked), "commands.execute.conditional.fail_count");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    let data = server._instance.path().join("world/data/minecraft");
    assert!(data.join("command_storage_test.dat").is_file());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn data_edits_block_entities_and_stores_numbers_into_nbt() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0 20 20").await;

    let plain = run(&server, "setblock 4 300 4 stone").await;
    assert_eq!(key(&plain), "commands.setblock.success");
    let invalid = run(&server, "data get block 4 300 4").await;
    assert_eq!(key(&invalid), "commands.data.block.invalid");
    let unloaded = run(&server, "data get block 4000 300 4000").await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");

    run(&server, "setblock 3 300 3 chest").await;
    let items = run(&server, "data get block 3 300 3 Items").await;
    assert_eq!(key(&items), "commands.data.block.query");
    assert_eq!(arg(&items, 3), "[]");
    let added = run(
        &server,
        r#"data modify block 3 300 3 Items append value {Slot:0b,id:"minecraft:stone",count:5}"#,
    )
    .await;
    assert_eq!(key(&added), "commands.data.block.modified");
    assert_eq!(arg(&added, 0), "3");
    let count = run(&server, "data get block 3 300 3 Items[0].count").await;
    assert_eq!(arg(&count, 3), "5");

    run(
        &server,
        "execute store result storage test:s n int 1 run data get block 3 300 3 Items[0].count",
    )
    .await;
    let n = run(&server, "data get storage test:s n").await;
    assert_eq!(arg(&n, 1), "5");

    run(
        &server,
        "execute store result block 3 300 3 Items[0].count int 2 run data get storage test:s n",
    )
    .await;
    let count = run(&server, "data get block 3 300 3 Items[0].count").await;
    assert_eq!(arg(&count, 3), "10");

    run(
        &server,
        "execute store success storage test:s ok byte 1 run data get storage test:s n",
    )
    .await;
    run(
        &server,
        "execute store result storage test:s half double 0.5 run data get storage test:s n",
    )
    .await;
    let stored = run(&server, "data get storage test:s").await;
    assert_eq!(arg(&stored, 1), "{n: 5, ok: 1b, half: 2.5d}");
    run(
        &server,
        "execute store success storage test:s ok byte 1 run data get storage test:s missing",
    )
    .await;
    let stored = run(&server, "data get storage test:s ok").await;
    assert_eq!(arg(&stored, 1), "0b");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn data_and_tag_edit_region_entities() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0 20 20").await;
    run(&server, "setblock 5 300 5 glass").await;
    run(
        &server,
        "summon pig 5 310 5 {NoAI:1b,NoGravity:1b,PersistenceRequired:1b}",
    )
    .await;
    run(
        &server,
        "summon cow 7 310 7 {NoAI:1b,NoGravity:1b,PersistenceRequired:1b}",
    )
    .await;

    let ai = run(
        &server,
        "data get entity @e[x=5,y=310,z=5,distance=..10,type=pig,limit=1] NoAI",
    )
    .await;
    assert_eq!(key(&ai), "commands.data.entity.query");
    assert_eq!(arg(&ai, 1), "1b");
    let named = run(
        &server,
        r#"data modify entity @e[x=5,y=310,z=5,distance=..10,type=pig,limit=1] CustomName set value "Bob""#,
    )
    .await;
    assert_eq!(key(&named), "commands.data.entity.modified");
    let name = run(
        &server,
        "data get entity @e[x=5,y=310,z=5,distance=..10,type=pig,limit=1] CustomName",
    )
    .await;
    assert_eq!(arg(&name, 1), r#""Bob""#);
    let same = run(
        &server,
        r#"data modify entity @e[x=5,y=310,z=5,distance=..10,type=pig,limit=1] CustomName set value "Bob""#,
    )
    .await;
    assert_eq!(key(&same), "commands.data.merge.failed");
    let still = run(
        &server,
        "execute if entity @e[x=5,y=310,z=5,distance=..10,type=pig]",
    )
    .await;
    assert_eq!(arg(&still, 0), "1");
    let health = run(
        &server,
        "data get entity @e[x=5,y=310,z=5,distance=..10,type=cow,limit=1] Health 10",
    )
    .await;
    assert_eq!(key(&health), "commands.data.entity.get");
    assert_eq!(arg(&health, 3), "100");

    let added = run(
        &server,
        "tag @e[x=5,y=310,z=5,distance=..10,type=pig] add boss",
    )
    .await;
    assert_eq!(key(&added), "commands.tag.add.success.single");
    assert_eq!(arg(&added, 0), "boss");
    let again = run(
        &server,
        "tag @e[x=5,y=310,z=5,distance=..10,type=pig] add boss",
    )
    .await;
    assert_eq!(key(&again), "commands.tag.add.failed");
    let both = run(&server, "tag @e[x=5,y=310,z=5,distance=..10] add shared").await;
    assert_eq!(key(&both), "commands.tag.add.success.multiple");
    assert_eq!(arg(&both, 1), "2");
    let found = run(
        &server,
        "execute if entity @e[x=5,y=310,z=5,distance=..10,tag=boss]",
    )
    .await;
    assert_eq!(arg(&found, 0), "1");
    let found = run(
        &server,
        "execute if entity @e[x=5,y=310,z=5,distance=..10,tag=shared]",
    )
    .await;
    assert_eq!(arg(&found, 0), "2");
    let list = run(&server, "tag @e[x=5,y=310,z=5,distance=..10,type=pig] list").await;
    assert_eq!(key(&list), "commands.tag.list.single.success");
    assert_eq!(arg(&list, 1), "2");
    assert_eq!(arg(&list, 2), "boss, shared");
    let saved = run(
        &server,
        "data get entity @e[x=5,y=310,z=5,distance=..10,type=pig,limit=1] Tags",
    )
    .await;
    assert_eq!(arg(&saved, 1), r#"["boss", "shared"]"#);
    let removed = run(
        &server,
        "tag @e[x=5,y=310,z=5,distance=..10,tag=boss] remove boss",
    )
    .await;
    assert_eq!(key(&removed), "commands.tag.remove.success.single");
    let missing = run(
        &server,
        "tag @e[x=5,y=310,z=5,distance=..10,tag=shared] remove boss",
    )
    .await;
    assert_eq!(key(&missing), "commands.tag.remove.failed");
    run(
        &server,
        "tag @e[x=5,y=310,z=5,distance=..10,tag=shared] remove shared",
    )
    .await;
    let empty = run(&server, "tag @e[x=5,y=310,z=5,distance=..10,type=cow] list").await;
    assert_eq!(key(&empty), "commands.tag.list.single.empty");
    let multi_empty = run(&server, "tag @e[x=5,y=310,z=5,distance=..10] list").await;
    assert_eq!(key(&multi_empty), "commands.tag.list.multiple.empty");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn data_modify_compute_sets_number_provider_values() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    let set = run(
        &server,
        "data modify storage test:c n set compute default integer {type:\"minecraft:uniform\",min:4,max:4}",
    )
    .await;
    assert_eq!(key(&set), "commands.data.storage.modified");
    let read = run(&server, "data get storage test:c n").await;
    assert_eq!(arg(&read, 1), "4");

    run(&server, "scoreboard objectives add points dummy").await;
    run(&server, "scoreboard players set #holder points 7").await;
    let float = run(
        &server,
        "data modify storage test:c f set compute default float {type:\"minecraft:from_int\",input:{type:\"minecraft:score\",target:{type:\"minecraft:fixed\",name:\"#holder\"},score:\"points\"}}",
    )
    .await;
    assert_eq!(key(&float), "commands.data.storage.modified", "{float:?}");
    let read = run(&server, "data get storage test:c f").await;
    assert_eq!(arg(&read, 1), "7.0f");

    run(&server, "data modify storage test:c l set value []").await;
    run(
        &server,
        "data modify storage test:c l append compute default integer 9",
    )
    .await;
    let read = run(&server, "data get storage test:c l").await;
    assert_eq!(arg(&read, 1), "[9]");

    let unknown = run(
        &server,
        "data modify storage test:c n set compute default integer test:missing",
    )
    .await;
    assert_eq!(key(&unknown), "argument.resource_or_id.no_such_element");
    let unloaded = run(
        &server,
        "data modify storage test:c n set compute block 5000 64 5000 integer 1",
    )
    .await;
    assert_eq!(key(&unloaded), "argument.pos.unloaded");
    let nobody = run(
        &server,
        "data modify storage test:c n set compute entity @p integer 1",
    )
    .await;
    assert_eq!(key(&nobody), "argument.entity.notfound.entity");

    run(&server, "forceload add 0 0 20 20").await;
    let placed = run(&server, "setblock 4 300 4 stone").await;
    assert_eq!(key(&placed), "commands.setblock.success", "{placed:?}");
    let cond = "{type:\"minecraft:conditional\",condition:{type:\"minecraft:block_state_property\",block:\"minecraft:stone\"},on_true:5,on_false:6}";
    let set = run(
        &server,
        &format!("data modify storage test:c b set compute block 4 300 4 integer {cond}"),
    )
    .await;
    assert_eq!(key(&set), "commands.data.storage.modified", "{set:?}");
    let read = run(&server, "data get storage test:c b").await;
    assert_eq!(arg(&read, 1), "5");
    run(
        &server,
        &format!("data modify storage test:c d set compute default integer {cond}"),
    )
    .await;
    let read = run(&server, "data get storage test:c d").await;
    assert_eq!(arg(&read, 1), "6");

    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn item_command_edits_containers_and_mobs() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 2 80 2 minecraft:chest").await;
    run(&server, "setblock 4 80 2 minecraft:stone").await;

    let set = run(
        &server,
        "item replace block 2 80 2 container.3 with minecraft:diamond 5",
    )
    .await;
    assert_eq!(key(&set), "commands.item.block.set.success");
    let copy = run(
        &server,
        "item replace block 2 80 2 container.4 from block 2 80 2 container.3",
    )
    .await;
    assert_eq!(key(&copy), "commands.item.block.set.success");
    let data = run(&server, "data get block 2 80 2 Items").await;
    let shown = arg(&data, 3);
    assert!(
        shown.contains("Slot: 3b") && shown.contains("Slot: 4b"),
        "{shown}"
    );
    let stone = run(
        &server,
        "item replace block 4 80 2 container.0 with minecraft:dirt",
    )
    .await;
    assert_eq!(key(&stone), "commands.item.target.not_a_container");
    let bad = run(
        &server,
        "item replace block 2 80 2 container.40 with minecraft:dirt",
    )
    .await;
    assert_eq!(key(&bad), "commands.item.target.no_such_slot");
    let cleared = run(
        &server,
        "item replace block 2 80 2 container.3 from block 2 80 2 container.20",
    )
    .await;
    assert_eq!(key(&cleared), "commands.item.block.set.success");

    run(
        &server,
        "summon minecraft:zombie 2 81 2 {Tags:[\"t\"],NoAI:1b}",
    )
    .await;
    let worn = run(
        &server,
        "item replace entity @e[tag=t] armor.head with minecraft:iron_helmet",
    )
    .await;
    assert_eq!(key(&worn), "commands.item.entity.set.success.single");
    let data = run(
        &server,
        "data get entity @e[tag=t,limit=1] equipment.head.id",
    )
    .await;
    assert!(arg(&data, 1).contains("iron_helmet"), "{:?}", data);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn execute_if_items_and_slots_count_container_contents() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 2 80 2 minecraft:chest").await;
    run(
        &server,
        "item replace block 2 80 2 container.3 with minecraft:diamond 5",
    )
    .await;
    run(
        &server,
        "item replace block 2 80 2 container.4 with minecraft:dirt 2",
    )
    .await;

    let diamonds = run(
        &server,
        "execute if items block 2 80 2 container.* minecraft:diamond",
    )
    .await;
    assert_eq!(key(&diamonds), "commands.execute.conditional.pass_count");
    assert_eq!(arg(&diamonds, 0), "5");
    let any = run(&server, "execute if items block 2 80 2 container.* *").await;
    assert_eq!(arg(&any, 0), "7");
    let none = run(
        &server,
        "execute if items block 2 80 2 container.* minecraft:emerald",
    )
    .await;
    assert_eq!(key(&none), "commands.execute.conditional.fail");
    let missing = run(
        &server,
        "execute unless items block 2 80 2 container.* minecraft:emerald",
    )
    .await;
    assert_eq!(key(&missing), "commands.execute.conditional.pass");
    let slots = run(&server, "execute if slots block 2 80 2 container.*").await;
    assert_eq!(arg(&slots, 0), "27");
    let hotbar = run(&server, "execute if slots block 2 80 2 container.3").await;
    assert_eq!(arg(&hotbar, 0), "1");
    let not_container = run(&server, "execute if items block 4 80 2 container.0 *").await;
    assert_ne!(
        key(&not_container),
        "commands.execute.conditional.pass_count"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn item_modify_runs_inline_modifiers_on_containers_and_mobs() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 2 80 2 minecraft:chest").await;
    run(
        &server,
        "item replace block 2 80 2 container.1 with minecraft:diamond 5",
    )
    .await;

    let modified = run(
        &server,
        "item modify block 2 80 2 container.1 {type:\"minecraft:set_count\",count:9}",
    )
    .await;
    assert_eq!(key(&modified), "commands.item.block.modify.success");
    let data = run(&server, "data get block 2 80 2 Items[0].count").await;
    assert_eq!(arg(&data, 3), "9");

    let empty = run(
        &server,
        "item modify block 2 80 2 container.2 {type:\"minecraft:set_count\",count:9}",
    )
    .await;
    assert_eq!(key(&empty), "commands.item.block.modify.success");
    let missing = run(
        &server,
        "item modify block 2 80 2 container.40 {type:\"minecraft:set_count\",count:9}",
    )
    .await;
    assert_eq!(key(&missing), "commands.item.target.no_such_slot");
    let unknown = run(&server, "item modify block 2 80 2 container.1 foo:missing").await;
    assert_eq!(key(&unknown), "argument.resource_or_id.no_such_element");
    let broken = run(
        &server,
        "item modify block 2 80 2 container.1 {type:\"minecraft:nope\"}",
    )
    .await;
    assert_eq!(key(&broken), "argument.resource_or_id.failed_to_parse");

    let copied = run(
        &server,
        "item replace block 2 80 2 container.5 from block 2 80 2 container.1 {type:\"minecraft:set_count\",count:2}",
    )
    .await;
    assert_eq!(key(&copied), "commands.item.block.set.success");
    let data = run(&server, "data get block 2 80 2 Items[1].count").await;
    assert_eq!(arg(&data, 3), "2");

    run(
        &server,
        "summon minecraft:zombie 2 81 2 {Tags:[\"t\"],NoAI:1b}",
    )
    .await;
    run(
        &server,
        "item replace entity @e[tag=t] armor.head with minecraft:iron_helmet",
    )
    .await;
    let worn = run(
        &server,
        "item modify entity @e[tag=t] armor.head {type:\"minecraft:set_custom_data\",tag:{x:1}}",
    )
    .await;
    assert_eq!(key(&worn), "commands.item.entity.modify.success.single");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn destroy_mode_drops_the_loot_of_blocks_needing_a_tool() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0 20 20").await;

    run(&server, "setblock 4 300 4 stone").await;
    run(&server, "setblock 4 300 4 air destroy").await;
    let cobble = run(
        &server,
        "execute if entity @e[type=item,nbt={Item:{id:\"minecraft:cobblestone\"}}]",
    )
    .await;
    assert_eq!(key(&cobble), "commands.execute.conditional.pass_count");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn item_command_reaches_single_item_and_shelf_containers() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 2 80 2 minecraft:jukebox").await;
    run(&server, "setblock 4 80 2 minecraft:decorated_pot").await;
    run(&server, "setblock 6 80 2 minecraft:chiseled_bookshelf").await;
    run(&server, "setblock 8 80 2 minecraft:lectern").await;

    for (x, slot) in [(2, "container.0"), (4, "container.0"), (6, "container.5")] {
        let set = run(
            &server,
            &format!("item replace block {x} 80 2 {slot} with minecraft:book"),
        )
        .await;
        assert_eq!(key(&set), "commands.item.block.set.success", "{x}");
    }
    let record = run(&server, "data get block 2 80 2 RecordItem.id").await;
    assert!(arg(&record, 3).contains("minecraft:book"), "{record:?}");
    let pot = run(&server, "data get block 4 80 2 item.id").await;
    assert!(arg(&pot, 3).contains("minecraft:book"), "{pot:?}");
    let shelf = run(&server, "data get block 6 80 2 Items[0].Slot").await;
    assert_eq!(arg(&shelf, 3), "5b");

    let past = run(
        &server,
        "item replace block 4 80 2 container.1 with minecraft:book",
    )
    .await;
    assert_eq!(key(&past), "commands.item.target.no_such_slot");
    let lectern = run(
        &server,
        "item replace block 8 80 2 container.0 with minecraft:book",
    )
    .await;
    assert_eq!(key(&lectern), "commands.item.target.not_a_container");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn bossbar_names_expand_nbt_contents_and_gamerule_and_compute_return_results() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;

    run(&server, "data merge storage test:r {s:\"expanded\"}").await;
    let created = run(&server, "bossbar add test:b {nbt:\"s\",storage:\"test:r\"}").await;
    let json = serde_json::to_string(&created).unwrap();
    assert!(json.contains("expanded"), "{json}");
    assert!(!json.contains("storage"), "{json}");

    let rule = server
        .handle
        .run_command("gamerule keep_inventory true")
        .await;
    assert!(rule.is_ok());
    let stored = run(
        &server,
        "execute store result storage test:r g int 1 run gamerule keep_inventory true",
    )
    .await;
    assert!(!stored.is_empty());
    let got = run(&server, "data get storage test:r g").await;
    assert_eq!(arg(&got, 1), "1");
    run(
        &server,
        "execute store result storage test:r g int 1 run gamerule keep_inventory false",
    )
    .await;
    let got = run(&server, "data get storage test:r g").await;
    assert_eq!(arg(&got, 1), "0");

    let computed = run(&server, "compute default integer 7").await;
    assert_eq!(key(&computed), "command.compute.result.unnamed.exact");
    assert_eq!(arg(&computed, 0), "7");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn item_and_execute_take_inline_and_stored_slot_sources() {
    let (_, session_url) = MockSessionServer::start().await;
    let server = start_server("127.0.0.1:0", &session_url).await;
    run(&server, "forceload add 0 0").await;
    run(&server, "setblock 2 80 2 minecraft:chest").await;
    for (slot, item) in [(1, "dirt"), (3, "diamond"), (5, "dirt"), (7, "dirt")] {
        run(
            &server,
            &format!("item replace block 2 80 2 container.{slot} with minecraft:{item}"),
        )
        .await;
    }
    let dirt = "{type:\"minecraft:filtered\",item_filter:{items:\"minecraft:dirt\"},slot_source:{type:\"minecraft:slot_range\",slots:\"container.*\"}}";
    let counted = run(&server, &format!("execute if slots block 2 80 2 {dirt}")).await;
    assert_eq!(arg(&counted, 0), "3");
    let limited = format!("{{type:\"minecraft:limit_slots\",limit:2,slot_source:{dirt}}}");
    run(
        &server,
        &format!("item modify block 2 80 2 {limited} {{type:\"minecraft:set_count\",count:9}}"),
    )
    .await;
    let nine = run(
        &server,
        "execute if items block 2 80 2 container.* minecraft:dirt",
    )
    .await;
    assert_eq!(arg(&nine, 0), "19");
    let group = "[{type:\"minecraft:slot_range\",slots:\"container.3\"},{type:\"minecraft:slot_range\",slots:\"container.7\"}]";
    let grouped = run(&server, &format!("execute if items block 2 80 2 {group} *")).await;
    assert_eq!(arg(&grouped, 0), "2");

    let empty = run(
        &server,
        "item replace block 2 80 2 {type:\"minecraft:empty\"} with minecraft:stone",
    )
    .await;
    assert_eq!(key(&empty), "commands.item.target.no_such_slot.unnamed");
    let unknown = run(
        &server,
        "item replace block 2 80 2 foo:missing with minecraft:stone",
    )
    .await;
    assert_eq!(key(&unknown), "argument.resource_or_id.no_such_element");
    let bad = run(
        &server,
        "item replace block 2 80 2 {type:\"minecraft:nope\"} with minecraft:stone",
    )
    .await;
    assert_eq!(key(&bad), "argument.resource_or_id.failed_to_parse");
}
