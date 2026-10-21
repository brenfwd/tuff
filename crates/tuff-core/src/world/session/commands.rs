use super::*;

impl PlayerSession {
    pub fn permission_subject(&self) -> crate::permission::Subject {
        crate::permission::Subject::Player {
            uuid: self.profile.id,
            level: self.mode.permission,
        }
    }

    /// The command source this player is.
    pub fn command_source(&self) -> Source {
        Source {
            kind: SourceKind::Player {
                id: self.id,
                uuid: self.profile.id,
                handle: self.conn.handle.clone(),
            },
            name: self.profile.name.clone(),
            permission: self.mode.permission,
            dimension: self.dimension,
            position: self.pose.position,
            rotation: [self.pose.yaw, self.pose.pitch],
            anchor: crate::command::args::Anchor::Feet,
            entity: Some(self.id),
            silent: false,
        }
    }

    /// Runs a clicked sign's commands as this player from the sign.
    pub async fn run_sign_commands(
        &mut self,
        dimension: crate::world::dimension::DimensionId,
        position: [f64; 3],
        commands: Vec<String>,
    ) {
        for command in commands {
            let mut source = self.command_source();
            source.permission = tuff_storage::admin::PermissionLevel::Gamemasters;
            source.dimension = dimension;
            source.position = position;
            source.rotation = [0.0, 0.0];
            source.silent = true;
            let input = command.strip_prefix('/').unwrap_or(&command);
            crate::world::commands::run(Actor::Player(self), source, input).await;
        }
    }

    pub async fn run_command(&mut self, input: &str) {
        let source = self.command_source();
        crate::world::commands::run(Actor::Player(self), source, input).await;
    }

    pub async fn suggest(&mut self, id: i32, text: &str) {
        let source = self.command_source();
        if let Some(found) =
            crate::world::commands::registry::claimed_suggestions(&self.shared, &source, text).await
        {
            self.send(&play::CommandSuggestions {
                id,
                start: found.start as i32,
                length: found.length as i32,
                matches: capped(&found.matches),
                tooltips: &[],
            });
            return;
        }
        let (reply, players) = oneshot::channel();
        self.coordinator(CoordinatorMessage::Players { reply })
            .await;
        let player_names: Vec<String> = players
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.name)
            .collect();
        let (objectives, teams) = self
            .shared
            .scoreboard
            .read(|board| {
                (
                    board.objectives().map(|o| o.name.clone()).collect(),
                    board.teams().map(|t| t.name.clone()).collect(),
                )
            })
            .await
            .unwrap_or_default();
        let context = SuggestContext {
            allow_selectors: self.mode.permission.allows_selectors(),
            player_names: player_names.clone(),
            dimensions: self
                .shared
                .dimensions
                .keys()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            objectives,
            teams,
            advancements: Some(self.progress.advancement_tree.clone()),
            tags: Some(self.shared.datapacks.contents().tags.clone()),
        };
        let dimension = self.dimension().key.clone();
        let commands = self.shared.commands.clone();
        let permissions = self.shared.permissions.view();
        let input = crate::world::commands::SuggestInput {
            dimension: &dimension,
            player_names: &player_names,
            config: &self.shared.config,
            tracer: &self.shared.tracer,
            datapacks: &self.shared.datapacks,
            permissions: &permissions,
            services: &self.shared.services,
        };
        let custom = |_provider: &str, path: &[crate::command::ParsedNode], partial: &str| {
            let command = path.first()?.node.name;
            commands.executor(command)?.suggest(path, partial, &input)
        };
        let suggestions = commands.roots().suggest(
            text,
            permissions.access(self.permission_subject()),
            &context,
            &custom,
        );
        let tooltips: Vec<Option<TextComponent>> = match text
            .strip_prefix('/')
            .and_then(|t| t.split(' ').next())
            .and_then(|name| commands.executor(name))
        {
            Some(executor) => suggestions
                .matches
                .iter()
                .map(|m| executor.describe(text, m, &input))
                .collect(),
            None => Vec::new(),
        };
        self.send(&play::CommandSuggestions {
            id,
            start: suggestions.start as i32,
            length: suggestions.length as i32,
            matches: capped(&suggestions.matches),
            tooltips: capped(&tooltips),
        });
    }
}

const MAX_COMMAND_SUGGESTIONS: usize = 1000;

fn capped<T>(items: &[T]) -> &[T] {
    &items[..items.len().min(MAX_COMMAND_SUGGESTIONS)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn suggestions_are_capped_at_one_thousand() {
        let many = vec![0u8; 1500];
        assert_eq!(capped(&many).len(), 1000);
        assert_eq!(capped(&many[..10]).len(), 10);
    }
}
