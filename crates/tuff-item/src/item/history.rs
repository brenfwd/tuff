//! In debug builds every counted stack carries an id and a log of the
//! operations applied to it: what happened, where in the source, and which
//! function did it. The lost-item report prints the log so a dropped stack
//! can be traced back through its life. Release builds keep neither.

use std::fmt;
#[cfg(debug_assertions)]
use std::panic::Location;

use tuff_vanilla_data::Item;

use super::{Components, Counted, ItemStack, Mode};

#[cfg(debug_assertions)]
const KEPT: usize = 64;

pub struct History {
    #[cfg(debug_assertions)]
    id: StackId,
    #[cfg(debug_assertions)]
    entries: Vec<Entry>,
}

#[cfg(debug_assertions)]
struct Entry {
    what: String,
    location: &'static Location<'static>,
    backtrace: std::backtrace::Backtrace,
}

/// A stack's id, shown as a hyphenated UUID. Debug builds only: in release
/// builds it is empty and formats as nothing, so log arguments naming it
/// cost nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackId(#[cfg(debug_assertions)] [u8; 16]);

impl fmt::Display for StackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        #[cfg(debug_assertions)]
        {
            let id = u128::from_be_bytes(self.0);
            write!(
                f,
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                (id >> 96) as u32,
                (id >> 80) as u16,
                (id >> 64) as u16,
                (id >> 48) as u16,
                id & 0xffff_ffff_ffff
            )
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = f;
            Ok(())
        }
    }
}

impl fmt::Debug for StackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl PartialEq for History {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for History {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.id(), f)
    }
}

impl History {
    #[track_caller]
    pub(super) fn begin(item: Item, count: u8, origin: fmt::Arguments<'_>) -> Self {
        #[cfg(debug_assertions)]
        let mut history = {
            let mut id: [u8; 16] = rand::random();
            id[6] = (id[6] & 0x0f) | 0x40;
            id[8] = (id[8] & 0x3f) | 0x80;
            Self {
                id: StackId(id),
                entries: Vec::new(),
            }
        };
        #[cfg(not(debug_assertions))]
        let mut history = Self {};
        history.record(format_args!("{origin}: {count} {}", item.name()));
        history
    }

    pub fn id(&self) -> StackId {
        #[cfg(debug_assertions)]
        {
            self.id
        }
        #[cfg(not(debug_assertions))]
        {
            StackId()
        }
    }

    #[track_caller]
    pub(super) fn record(&mut self, what: fmt::Arguments<'_>) {
        #[cfg(debug_assertions)]
        {
            if self.entries.len() == KEPT {
                self.entries.remove(0);
            }
            self.entries.push(Entry {
                what: what.to_string(),
                location: Location::caller(),
                backtrace: std::backtrace::Backtrace::force_capture(),
            });
        }
        #[cfg(not(debug_assertions))]
        let _ = what;
    }

    /// The log as text, oldest entry first; nothing beyond "item" outside
    /// debug builds.
    pub fn report(&self) -> String {
        #[cfg_attr(
            not(debug_assertions),
            expect(unused_mut, reason = "only debug builds record the trail")
        )]
        let mut out = String::from("item");
        #[cfg(debug_assertions)]
        {
            use fmt::Write;
            let _ = write!(out, " {} ({} operations kept)", self.id, self.entries.len());
            for (i, entry) in self.entries.iter().enumerate() {
                let _ = write!(
                    out,
                    "\n  {}. {} at {}:{} in {}",
                    i + 1,
                    entry.what,
                    entry.location.file(),
                    entry.location.line(),
                    entry.function()
                );
            }
        }
        out
    }
}

#[cfg(debug_assertions)]
impl Entry {
    /// The function at the recorded location, read off the frame whose
    /// source position matches it.
    fn function(&self) -> String {
        let text = self.backtrace.to_string();
        let lines: Vec<&str> = text.lines().collect();
        let wanted = format!("{}:{}:", self.location.file(), self.location.line());
        lines
            .windows(2)
            .find_map(|pair| {
                let at = pair[1].trim().strip_prefix("at ")?;
                at.contains(&wanted).then_some(pair[0])
            })
            .map(|frame| {
                let frame = frame.trim();
                let name = frame.split_once(": ").map_or(frame, |(number, name)| {
                    if number.bytes().all(|b| b.is_ascii_digit()) {
                        name
                    } else {
                        frame
                    }
                });
                name.to_owned()
            })
            .unwrap_or_else(|| "?".to_owned())
    }
}

impl<M: Mode> ItemStack<M> {
    /// The only place a stack is put together; everything else goes
    /// through a door that calls this.
    #[track_caller]
    pub(crate) fn assemble(
        item: Item,
        count: u8,
        components: Components<M>,
        origin: fmt::Arguments<'_>,
    ) -> Self {
        Self {
            item,
            count,
            components,
            extra: M::begin(item, count, origin),
        }
    }

    #[track_caller]
    pub(crate) fn note(&mut self, what: fmt::Arguments<'_>) {
        M::record(&mut self.extra, what);
    }
}

impl ItemStack<Counted> {
    pub fn history(&self) -> &History {
        &self.extra
    }
}
