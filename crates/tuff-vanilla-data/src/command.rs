//! The server's command tree, rooted at [`ROOT`].
//!
//! An argument's parser properties and a node's permission requirement (its
//! `require`/`command_level`/... predicate) are kept as raw JSON text; see
//! the [module-level rationale](crate) this crate applies throughout for
//! Minecraft's smaller open-ended sub-languages.

#[derive(Clone, Copy, Debug)]
pub struct CommandNode {
    /// This node's literal or argument name; empty for the root.
    pub name: &'static str,
    pub kind: CommandNodeKind,
    /// Whether the command is valid ending here (as opposed to requiring
    /// more arguments).
    pub executable: bool,
    /// Path (from the root) of the node this one redirects to, e.g.
    /// `["teleport"]` for `tp`. `Some(&[])` redirects to the root itself, as
    /// `execute ... run <command>` does.
    pub redirect: Option<&'static [&'static str]>,
    /// Raw JSON of the permission requirement beyond the parent's, if any.
    pub permissions: Option<&'static str>,
    pub children: &'static [CommandNode],
}

#[derive(Clone, Copy, Debug)]
pub enum CommandNodeKind {
    Root,
    Literal,
    Argument {
        /// The argument-type parser's resource location, e.g. `brigadier:integer`.
        parser: &'static str,
        /// Raw JSON of the parser's properties (e.g. `brigadier:integer`'s
        /// `min`/`max`), if it takes any.
        properties: Option<&'static str>,
    },
}

impl CommandNode {
    /// This node's child named `name`, if any.
    pub fn child(&self, name: &str) -> Option<&'static CommandNode> {
        self.children.iter().find(|child| child.name == name)
    }

    /// Walks `path` (as in [`CommandNode::redirect`]) from the root.
    pub fn resolve(
        path: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Option<&'static CommandNode> {
        let mut node = &ROOT;
        for name in path {
            node = node.child(name.as_ref())?;
        }
        Some(node)
    }

    /// The node this one redirects to, if any.
    pub fn redirect_target(&self) -> Option<&'static CommandNode> {
        Self::resolve(self.redirect?)
    }
}

include!(concat!(env!("OUT_DIR"), "/commands.rs"));
