// SPDX-License-Identifier: MIT
use crate::edit::{self, Job, Op};
use crate::history::commit;
use crate::region::Region;
use crate::session::{model, say, tell};

const MAX_JOBS: usize = 8;
const REPORT_EVERY: u32 = 20;

pub fn busy(id: u128) -> Option<String> {
    model(|m| {
        if m.jobs.iter().any(|j| j.owner == id) {
            Some("You already have an edit running".to_owned())
        } else if m.jobs.len() >= MAX_JOBS {
            Some("Too many edits are running; try again shortly".to_owned())
        } else {
            None
        }
    })
}

pub fn start(job: Job) -> i32 {
    let total = job.total;
    let kind = job.kind_name();
    model(|m| {
        m.jobs.push_back(job);
        if !m.timer {
            tuff_plugin_sdk::schedule::after(1, tick);
            m.timer = true;
        }
    });
    say(format!("Queued {kind}: {total} blocks"))
}

pub fn edit(id: u128, op: Op, dim: String, region: Region, volume: u64) -> i32 {
    if let Some(why) = busy(id) {
        return say(why);
    }
    let (a, b) = region.bounds();
    let gmask = model(|m| m.players[&id].gmask.clone());
    start(
        Job::new(id, dim, op, a, b, volume)
            .within(region)
            .gmask(gmask),
    )
}

pub fn tick() {
    let mut jobs = model(|m| core::mem::take(&mut m.jobs));
    let mut finished = Vec::new();
    for _ in 0..jobs.len() {
        let mut job = jobs.pop_front().unwrap();
        match job.step() {
            edit::Step::More => {
                job.ticks += 1;
                if job.ticks % REPORT_EVERY == 0 {
                    tell(
                        job.owner,
                        &format!(
                            "[WorldEdit] {}: {}/{} blocks",
                            job.kind_name(),
                            job.done,
                            job.total
                        ),
                    );
                }
                jobs.push_back(job);
            }
            edit::Step::Done => {
                tell(
                    job.owner,
                    &format!(
                        "[WorldEdit] {} finished: {} blocks changed",
                        job.kind_name(),
                        job.changed
                    ),
                );
                finished.push(job);
            }
            edit::Step::Failed(why) => {
                tell(
                    job.owner,
                    &format!("[WorldEdit] {} failed: {why}", job.kind_name()),
                );
            }
        }
    }
    let more = !jobs.is_empty();
    model(|m| {
        m.jobs = jobs;
        m.timer = more;
        if more {
            tuff_plugin_sdk::schedule::after(1, tick);
        }
    });
    for job in finished {
        commit(job);
    }
}
