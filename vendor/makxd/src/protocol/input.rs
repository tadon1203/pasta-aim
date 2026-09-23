use crate::types::{CommandApi, ControllerState};

pub(crate) enum InputCommand {
    MouseMove { x: i16, y: i16 },
    ControllerState(ControllerState),
}

pub(crate) struct EncodedCommand {
    pub command: u8,
    pub payload: Vec<u8>,
}

pub(crate) fn encode_input_command(api: CommandApi, command: InputCommand) -> EncodedCommand {
    match api {
        CommandApi::Mak => encode_mak(command),
        CommandApi::Km => encode_km(command),
    }
}

fn encode_mak(command: InputCommand) -> EncodedCommand {
    match command {
        InputCommand::MouseMove { x, y } => EncodedCommand {
            command: 0x18,
            payload: [x.to_le_bytes(), y.to_le_bytes()].concat(),
        },
        InputCommand::ControllerState(state) => EncodedCommand {
            command: 0x40,
            payload: controller_state_payload(state),
        },
    }
}

fn encode_km(command: InputCommand) -> EncodedCommand {
    let payload = match command {
        InputCommand::MouseMove { x, y } => format!("m.move({x},{y})").into_bytes(),
        InputCommand::ControllerState(state) => format!(
            "m.controller_state({},{},{},{},{},{},{},{})",
            state.digital_low,
            state.digital_high,
            state.left_trigger,
            state.right_trigger,
            state.left_stick_x,
            state.left_stick_y,
            state.right_stick_x,
            state.right_stick_y,
        )
        .into_bytes(),
    };
    EncodedCommand {
        command: b'k',
        payload,
    }
}

fn controller_state_payload(state: ControllerState) -> Vec<u8> {
    let mut payload = Vec::with_capacity(20);
    payload.extend_from_slice(&state.digital_low.to_le_bytes());
    payload.extend_from_slice(&state.digital_high.to_le_bytes());
    payload.extend_from_slice(&state.left_trigger.to_le_bytes());
    payload.extend_from_slice(&state.right_trigger.to_le_bytes());
    payload.extend_from_slice(&state.left_stick_x.to_le_bytes());
    payload.extend_from_slice(&state.left_stick_y.to_le_bytes());
    payload.extend_from_slice(&state.right_stick_x.to_le_bytes());
    payload.extend_from_slice(&state.right_stick_y.to_le_bytes());
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mak_move_matches_existing_payload() {
        let encoded =
            encode_input_command(CommandApi::Mak, InputCommand::MouseMove { x: 12, y: -7 });
        assert_eq!(encoded.command, 0x18);
        assert_eq!(encoded.payload, [12, 0, 249, 255]);
    }

    #[test]
    fn km_move_is_ascii_without_newline() {
        let encoded =
            encode_input_command(CommandApi::Km, InputCommand::MouseMove { x: 12, y: -7 });
        assert_eq!(encoded.command, b'k');
        assert_eq!(encoded.payload, b"m.move(12,-7)");
    }

    #[test]
    fn controller_state_uses_decimal_command_order() {
        let state = ControllerState {
            digital_low: 1,
            digital_high: 2,
            left_trigger: 3,
            right_trigger: 4,
            left_stick_x: -5,
            left_stick_y: 6,
            right_stick_x: -7,
            right_stick_y: 8,
        };
        let encoded = encode_input_command(CommandApi::Km, InputCommand::ControllerState(state));
        assert_eq!(encoded.payload, b"m.controller_state(1,2,3,4,-5,6,-7,8)");
    }
}
