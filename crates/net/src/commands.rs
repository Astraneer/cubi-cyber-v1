//! Общие подсказки команд; окончательные права и аргументы проверяет сервер.
use crate::Role;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: &'static str,
    pub usage: &'static str,
    pub minimum_role: Role,
}
impl CommandInfo {
    pub const fn allowed(self, role: Role) -> bool {
        role as u8 >= self.minimum_role as u8
    }
}
pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/gamemode",
        usage: "/gamemode survival|creative|spectator",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/tp",
        usage: "/tp <x> <y> <z>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/give",
        usage: "/give <item_id> <count>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/ban",
        usage: "/ban <name>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/unban",
        usage: "/unban <name>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/op",
        usage: "/op <name>",
        minimum_role: Role::Owner,
    },
    CommandInfo {
        name: "/deop",
        usage: "/deop <name>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/rollback",
        usage: "/rollback <tx_id>",
        minimum_role: Role::Admin,
    },
    CommandInfo {
        name: "/space_move",
        usage: "/space_move <space_id> <x> <y> <z>",
        minimum_role: Role::Admin,
    },
];
pub fn command_catalog() -> &'static [CommandInfo] {
    COMMANDS
}
pub fn commands_for(role: Role) -> impl Iterator<Item = &'static CommandInfo> {
    COMMANDS.iter().filter(move |command| command.allowed(role))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_respects_administrator_and_owner_roles() {
        assert_eq!(commands_for(Role::Player).count(), 0);
        assert!(!commands_for(Role::Admin).any(|command| command.name == "/op"));
        assert!(commands_for(Role::Owner).any(|command| command.name == "/op"));
        for (index, command) in COMMANDS.iter().enumerate() {
            assert!(command.usage.starts_with(command.name));
            assert!(!COMMANDS[..index].iter().any(|old| old.name == command.name));
        }
    }
}
