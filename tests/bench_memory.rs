#[path = "../benches/common/alloc.rs"]
mod alloc;
#[expect(dead_code, reason = "helpers used by only some targets")]
#[path = "../benches/stress/heap.rs"]
mod heap;
#[expect(dead_code, reason = "helpers used by only some targets")]
#[path = "../benches/common/memory.rs"]
mod memory;

const VMMAP: &str = "\
Process:         bash [23026]
Physical footprint:         1376K
Physical footprint (peak):  2.5M
----

                                VIRTUAL RESIDENT    DIRTY  SWAPPED VOLATILE   NONVOL    EMPTY   REGION
REGION TYPE                        SIZE     SIZE     SIZE     SIZE     SIZE     SIZE     SIZE    COUNT (non-coalesced)
===========                     ======= ========    =====  ======= ========   ======    =====  =======
Malloc Small                      8208K      96K      96K       0K       0K       0K       0K        4         see MALLOC ZONE table below
Stack                             8176K      32K      32K       0K       0K       0K       0K        1
VM_ALLOCATE                       1.2G     300M     250.5M       0K       0K       0K       0K       12
===========                     ======= ========    =====  ======= ========   ======    =====  =======
TOTAL                            386.8G   265.3M    1392K       0K       0K       0K       0K      327
TOTAL, minus reserved VM space   386.7G   265.0M    1390K       0K       0K       0K       0K      327

                                 VIRTUAL   RESIDENT      DIRTY    SWAPPED ALLOCATION      BYTES DIRTY+SWAP          REGION
MALLOC ZONE                         SIZE       SIZE       SIZE       SIZE      COUNT  ALLOCATED  FRAG SIZE  % FRAG   COUNT
===========                      =======  =========  =========  =========  =========  =========  =========  ======  ======
DefaultMallocZone_0x104664000      17.2M       416K       416K         0K        732        39K       377K     91%       6
===========                      =======  =========  =========  =========  =========  =========  =========  ======  ======
TOTAL                              17.2M       448K       448K         0K        868       3264       406K     91%       8
";

/// Tests that read the process-wide allocator counters run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn close(value: &serde_json::Value, expected: f64) -> bool {
    value.as_f64().is_some_and(|v| (v - expected).abs() < 1e-6)
}

#[tuff_core::watchdog]
#[test]
fn vmmap_sizes_parse_into_mib() {
    assert_eq!(memory::size_mib("1024K"), Some(1.0));
    assert_eq!(memory::size_mib("1.5G"), Some(1536.0));
    assert_eq!(memory::size_mib("265.3M"), Some(265.3));
    assert_eq!(memory::size_mib("1048576"), Some(1.0));
    assert_eq!(memory::size_mib("12x"), None);
    assert_eq!(memory::size_mib(""), None);
}

#[tuff_core::watchdog]
#[test]
fn vmmap_summary_splits_footprint_regions_and_malloc_zones() {
    let parsed = memory::parse_vmmap(VMMAP).unwrap();
    assert!(close(&parsed["footprint_mib"], 1376.0 / 1024.0));
    assert!(close(&parsed["footprint_peak_mib"], 2.5));
    assert!(close(&parsed["regions"]["resident_mib"], 265.3));
    assert!(close(&parsed["regions"]["dirty_mib"], 1392.0 / 1024.0));
    assert!(close(&parsed["malloc"]["resident_mib"], 448.0 / 1024.0));
    assert!(close(
        &parsed["malloc"]["allocated_mib"],
        3264.0 / 1024.0 / 1024.0
    ));
    assert_eq!(parsed["malloc"]["allocations"], 868);
    assert!(close(&parsed["malloc"]["frag_pct"], 91.0));
    let dirtiest = parsed["dirtiest"].as_array().unwrap();
    let names: Vec<&str> = dirtiest
        .iter()
        .map(|r| r["region"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["VM_ALLOCATE", "Malloc Small", "Stack"]);
    assert!(close(&dirtiest[0]["dirty_mib"], 250.5));
    assert!(close(&dirtiest[0]["resident_mib"], 300.0));
}

#[tuff_core::watchdog]
#[test]
fn vmmap_without_a_footprint_is_rejected() {
    assert!(memory::parse_vmmap("Process: bash\n").is_none());
}

#[tuff_core::watchdog]
#[test]
fn the_bench_allocator_counts_live_bytes() {
    let _serial = serial();
    const BYTES: usize = 64 << 20;
    let before = alloc::live_bytes();
    let mut block = vec![0u8; BYTES];
    block[BYTES - 1] = 1;
    let held = alloc::live_bytes();
    block.reserve_exact(BYTES);
    let grown = alloc::live_bytes();
    drop(std::hint::black_box(block));
    let after = alloc::live_bytes();
    let slack = 4 << 20;
    assert!(held >= before + BYTES as u64 - slack, "{before} -> {held}");
    assert!(grown >= held + BYTES as u64 - slack, "{held} -> {grown}");
    assert!(
        after + 2 * BYTES as u64 - slack <= grown,
        "{grown} -> {after}"
    );
}

#[tuff_core::watchdog]
#[test]
fn sampled_sites_follow_live_allocations() {
    let _serial = serial();
    const BYTES: usize = 8 << 20;
    alloc::sample_every(64 << 10);
    let held = std::hint::black_box(vec![1u8; BYTES]);
    let big = |sites: &[alloc::Site]| {
        sites
            .iter()
            .filter(|site| site.bytes >= BYTES as f64 && site.stack.iter().any(|&ip| ip != 0))
            .count()
    };
    assert!(big(&alloc::live_sites()) >= 1, "the block was sampled");
    drop(held);
    let left = big(&alloc::live_sites());
    alloc::sample_every(0);
    assert_eq!(left, 0, "and forgotten once freed");
}

#[tuff_core::watchdog]
#[test]
fn mangled_frames_read_as_paths() {
    assert_eq!(
        heap::readable(
            "_RNvMs3_NtNtCslgYHqXTRsdj_9tuff_core5world8craftingNtB5_9RecipeSet13from_registry \
             (in stress-1) (mod.rs:660)"
        ),
        "tuff_core::world::crafting::RecipeSet::from_registry (mod.rs:660)"
    );
    assert_eq!(
        heap::readable("_RNvCs56Xt4cN4w0a_7___rustc14___rust_realloc (in stress-1) + 12"),
        "__rustc::__rust_realloc"
    );
    assert_eq!(heap::readable("0x19e4f1cff"), "0x19e4f1cff");
}

#[tuff_core::watchdog]
#[test]
fn recipe_book_startup_heap_stays_small() {
    let _serial = serial();
    let mut registries = tuff_core::registry::Registries::new();
    tuff_core::feature::load_all(&tuff::vanilla_features::all(), &mut registries)
        .expect("the vanilla plugins load");
    let before = alloc::live_bytes();
    let set = tuff_core::world::crafting::RecipeSet::from_registry(&registries.recipe_entries);
    let held = alloc::live_bytes().saturating_sub(before);
    drop(std::hint::black_box(set));
    eprintln!("recipe set live heap: {held} bytes");
    assert!(held < 4 << 20, "recipe set holds {held} bytes");
}
