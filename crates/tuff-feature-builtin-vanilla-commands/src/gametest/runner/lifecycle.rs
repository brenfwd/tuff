use super::*;

impl Run {
    pub(super) async fn start_test(&mut self, world: &mut WorldAccess, i: usize) {
        self.infos[i].started = true;
        self.infos[i].timer = Some(Instant::now());
        self.set_status(world, i, Status::Running, None).await;
        let instance = self.infos[i].spec.instance.clone();
        match instance.kind {
            TestKind::Function => {
                let id = instance.function.clone().unwrap_or_default();
                match super::super::registry::test_function(&id) {
                    Some(function) => {
                        self.call(
                            world,
                            i,
                            CallbackKind::Start(Box::new(move |h: &mut TestHelper| function(h))),
                        )
                        .await;
                    }
                    None => {
                        self.infos[i].error = Some(TextComponent::translate(
                            "test.error.unknown",
                            [TextComponent::new(format!(
                                "Trying to access missing test function: {id}"
                            ))],
                        ))
                    }
                }
            }
            TestKind::BlockBased => self.start_block_based(world, i).await,
        }
    }

    pub(super) async fn start_block_based(&mut self, world: &mut WorldAccess, i: usize) {
        let Some(geometry) = self.infos[i].geometry else {
            return;
        };
        let dimension = self.infos[i].spec.dimension;
        let (low, high) = geometry.structure_box();
        let mut positions = Vec::new();
        for z in low[2]..=high[2] {
            for y in low[1]..=high[1] {
                for x in low[0]..=high[0] {
                    positions.push([x, y, z]);
                }
            }
        }
        let states = world.states(dimension, &positions).await;
        let mut found: HashMap<&str, Vec<[i32; 3]>> = HashMap::new();
        for (pos, state) in positions.iter().zip(states) {
            if let Some(state) = state.filter(|s| s.block() == Block::TestBlock)
                && let Some(mode) = state.get("mode")
            {
                found.entry(mode).or_default().push(*pos);
            }
        }
        let mode = |name: &str| TextComponent::translate(format!("test_block.mode.{name}"), []);
        let starts = found.remove("start").unwrap_or_default();
        if starts.is_empty() {
            self.infos[i].fail(TextComponent::translate(
                "test_block.error.missing",
                [mode("start")],
            ));
            return;
        }
        if starts.len() != 1 {
            self.infos[i].fail(TextComponent::translate(
                "test_block.error.too_many",
                [mode("start")],
            ));
            return;
        }
        world.test_blocks(dimension, &starts, true, false).await;
        self.infos[i].block_tests = Some(BlockTests {
            accept: found.remove("accept").unwrap_or_default(),
            fail: found.remove("fail").unwrap_or_default(),
            log: found.remove("log").unwrap_or_default(),
        });
    }

    pub(super) async fn check_test_blocks(&mut self, world: &mut WorldAccess, i: usize) {
        let dimension = self.infos[i].spec.dimension;
        let Some(tests) = self.infos[i].block_tests.as_ref() else {
            return;
        };
        let (accept, fail, log) = (tests.accept.clone(), tests.fail.clone(), tests.log.clone());
        if accept.is_empty() {
            self.infos[i].fail(TextComponent::translate(
                "test_block.error.missing",
                [TextComponent::translate("test_block.mode.accept", [])],
            ));
            return;
        }
        if world
            .test_blocks(dimension, &accept, false, false)
            .await
            .contains(&true)
        {
            self.succeed(world, i).await;
            return;
        }
        let failed = world.test_blocks(dimension, &fail, false, false).await;
        for (pos, triggered) in fail.iter().zip(failed) {
            if !triggered {
                continue;
            }
            let message = world
                .block_entity(dimension, *pos)
                .await
                .and_then(|(_, nbt)| nbt.and_then(|n| n.get_str("message").map(str::to_owned)))
                .unwrap_or_default();
            world.test_blocks(dimension, &[*pos], false, true).await;
            self.infos[i].fail(TextComponent::new(message));
            return;
        }
        let logged = world.test_blocks(dimension, &log, false, false).await;
        let triggered: Vec<[i32; 3]> = log
            .iter()
            .zip(logged)
            .filter(|(_, t)| *t)
            .map(|(p, _)| *p)
            .collect();
        world.test_blocks(dimension, &triggered, true, false).await;
        world.test_blocks(dimension, &triggered, false, true).await;
    }

    pub(super) async fn on_passed(
        &mut self,
        world: &mut WorldAccess,
        registry: &TestRegistry,
        i: usize,
    ) {
        let lineage = self.infos[i].lineage;
        self.lineages[lineage].successes += 1;
        let info = &self.infos[i];
        if info.spec.retry.has_retries() {
            self.handle_retry(world, i, true).await;
        } else if info.spec.instance.max_attempts <= 1 {
            let text = format!(
                "{} passed! ({}ms / {}gameticks)",
                info.spec.id, info.run_ms, info.tick
            );
            self.report_passed(world, i, &text).await;
        } else {
            let (attempts, successes) = (
                self.lineages[lineage].attempts,
                self.lineages[lineage].successes,
            );
            if successes >= info.spec.instance.required_successes {
                let text = format!(
                    "{} passed {successes} times of {attempts} attempts.",
                    info.spec.id
                );
                self.report_passed(world, i, &text).await;
            } else {
                let text = format!(
                    "Flaky test {} succeeded, attempt: {attempts} successes: {successes}",
                    info.spec.id
                );
                world.say("green", &text).await;
                self.rerun_test(world, registry, i).await;
            }
        }
        self.remove_barriers(world, i).await;
        self.completed(world, registry).await;
    }

    pub(super) async fn on_failed(
        &mut self,
        world: &mut WorldAccess,
        registry: &TestRegistry,
        i: usize,
    ) {
        let lineage = self.infos[i].lineage;
        let info = &self.infos[i];
        if info.spec.instance.max_attempts <= 1 {
            let error = info.error.clone().unwrap_or_default();
            self.report_failure(world, i, error).await;
            if self.infos[i].spec.retry.has_retries() {
                self.handle_retry(world, i, false).await;
            }
        } else {
            let (attempts, successes) = (
                self.lineages[lineage].attempts,
                self.lineages[lineage].successes,
            );
            let instance = info.spec.instance.clone();
            let mut text = format!(
                "Flaky test {} failed, attempt: {attempts}/{}",
                info.spec.id, instance.max_attempts
            );
            if instance.required_successes > 1 {
                text.push_str(&format!(
                    ", successes: {successes} ({} required)",
                    instance.required_successes
                ));
            }
            world.say("yellow", &text).await;
            if instance.max_attempts - attempts + successes >= instance.required_successes {
                self.rerun_test(world, registry, i).await;
            } else {
                let error = TextComponent::new(format!(
                    "Not enough successes: {successes} out of {attempts} attempts. Required successes: {}. max attempts: {}.",
                    instance.required_successes, instance.max_attempts
                ));
                self.infos[i].error = Some(error.clone());
                self.report_failure(world, i, error).await;
            }
        }
        self.failed_tests.insert(self.infos[i].spec.id.clone());
        if self.halt_on_error {
            self.end_environment(world).await;
            self.unforce(world).await;
            self.stopped = true;
            self.current.clear();
            self.summarize(world).await;
            return;
        }
        self.completed(world, registry).await;
    }

    pub(super) async fn handle_retry(&mut self, world: &mut WorldAccess, i: usize, passed: bool) {
        let lineage = &self.lineages[self.infos[i].lineage];
        let (attempts, successes) = (lineage.attempts, lineage.successes);
        let retry = self.infos[i].spec.retry;
        let mut status = format!(
            "[Run: {attempts:4}, Ok: {successes:4}, Fail: {:4}",
            attempts - successes
        );
        if !retry.unlimited() {
            status.push_str(&format!(", Left: {:4}", retry.tries - attempts));
        }
        status.push(']');
        let info = &self.infos[i];
        let name = format!(
            "{} {}! {}ms",
            info.spec.id,
            if passed { "passed" } else { "failed" },
            info.run_ms
        );
        let text = format!("{status:<53}{name}");
        if passed {
            self.report_passed(world, i, &text).await;
        } else {
            world.say("red", &text).await;
        }
        if retry.has_tries_left(attempts, successes) {
            self.pending_rerun.push(i);
        }
    }

    pub(super) async fn rerun_test(
        &mut self,
        _world: &mut WorldAccess,
        _registry: &TestRegistry,
        i: usize,
    ) {
        self.pending_rerun.push(i);
    }

    pub(super) async fn report_passed(&mut self, world: &mut WorldAccess, i: usize, text: &str) {
        self.set_status(world, i, Status::Finished, None).await;
        world.say("green", text).await;
        if let Some(junit) = &mut self.junit {
            let info = &self.infos[i];
            junit.passed(&info.spec.id, &info.spec.instance.structure, info.run_ms);
        }
    }

    pub(super) async fn report_failure(
        &mut self,
        world: &mut WorldAccess,
        i: usize,
        error: TextComponent,
    ) {
        self.set_status(world, i, Status::Finished, Some(error.clone()))
            .await;
        let info = &self.infos[i];
        let required = info.spec.instance.required;
        let message = error.plain();
        let line = format!(
            "{}{} failed! {message}",
            if required { "" } else { "(optional) " },
            info.spec.id
        );
        world
            .say(if required { "red" } else { "yellow" }, &line)
            .await;
        if required {
            tuff_core::log::error!("{} failed at {}! {message}", info.spec.id, info.pos_text());
        } else {
            tuff_core::log::warn!(
                "(optional) {} failed at {}. {message}",
                info.spec.id,
                info.pos_text()
            );
        }
        if let Some(junit) = &mut self.junit {
            junit.failed(
                &info.spec.id,
                &info.spec.instance.structure,
                info.run_ms,
                required,
                &format!("({}) {message}", info.pos_text()),
            );
        }
    }

    /// After a test finished: queue reruns, start the next batch once
    /// this one is done, and report when everything is.
    pub(super) async fn completed(&mut self, world: &mut WorldAccess, registry: &TestRegistry) {
        for i in std::mem::take(&mut self.pending_rerun) {
            let lineage = self.infos[i].lineage;
            let spec = self.infos[i].spec.clone();
            let mut copy = spec;
            copy.test_pos = self.infos[i].pos;
            let new = self.add(copy, Some(lineage));
            self.infos[new].pos = self.infos[i].pos;
            self.rerun.push(new);
        }
        let batch_done = self
            .batch_infos
            .get(self.batch)
            .is_some_and(|list| list.iter().all(|i| self.infos[*i].done));
        if self.is_done() && self.rerun.is_empty() {
            self.summarize(world).await;
        }
        if batch_done && !self.stopped {
            self.current.clear();
            Box::pin(self.batch_finished(world, registry)).await;
        } else if self.stopped && !self.rerun.is_empty() {
            Box::pin(self.run_scheduled_reruns(world, registry)).await;
        }
    }

    pub(super) async fn summarize(&mut self, world: &mut WorldAccess) {
        if self.summarized {
            return;
        }
        if !self.is_done() && !self.stopped {
            return;
        }
        self.summarized = true;
        let summary = self.summary();
        match &self.reporter {
            Reporter::Source { target, name } => {
                let shared = world.shared.clone();
                feedback::success(
                    &shared,
                    target,
                    name.clone(),
                    TextComponent::translate(
                        "commands.test.summary",
                        [TextComponent::new(summary.total.to_string())],
                    )
                    .color("white"),
                    true,
                )
                .await;
                if summary.failed_required.is_empty() {
                    feedback::success(
                        &shared,
                        target,
                        name.clone(),
                        TextComponent::translate("commands.test.summary.all_required_passed", [])
                            .color("green"),
                        true,
                    )
                    .await;
                } else {
                    feedback::fail(
                        target,
                        TextComponent::translate(
                            "commands.test.summary.failed",
                            [TextComponent::new(
                                summary.failed_required.len().to_string(),
                            )],
                        ),
                    );
                }
                if !summary.failed_optional.is_empty() {
                    target.send(TextComponent::translate(
                        "commands.test.summary.optional_failed",
                        [TextComponent::new(
                            summary.failed_optional.len().to_string(),
                        )],
                    ));
                }
            }
            Reporter::Log => {
                tuff_core::log::info!("{}", self.progress_bar());
                tuff_core::log::info!(
                    "========= {} GAME TESTS COMPLETE IN {:.3?} ======================",
                    summary.total,
                    self.started.elapsed()
                );
                if summary.failed_required.is_empty() {
                    tuff_core::log::info!("All {} required tests passed :)", summary.total);
                } else {
                    tuff_core::log::info!(
                        "{} required tests failed :(",
                        summary.failed_required.len()
                    );
                    for line in &summary.failed_required {
                        tuff_core::log::info!("   - {line}");
                    }
                }
                if !summary.failed_optional.is_empty() {
                    tuff_core::log::info!(
                        "{} optional tests failed",
                        summary.failed_optional.len()
                    );
                    for line in &summary.failed_optional {
                        tuff_core::log::info!("   - {line}");
                    }
                }
                tuff_core::log::info!("====================================================");
            }
        }
        if let Some(junit) = self.junit.take()
            && let Err(err) = junit.finish()
        {
            tuff_core::log::error!("Couldn't save test report: {err}");
        }
        if let Some(done) = self.done.take() {
            let _ = done.send(summary);
        }
    }
}
