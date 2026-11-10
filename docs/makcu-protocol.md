# MAKCU protocol

The part of the MAKCU binary protocol that the client uses. The code is in `NiceAimassist/Makcu/Protocol.cs` (encoding), `NiceAimassist/Makcu/PacketDecoder.cs` (parsing), and `NiceAimassist/Makcu/MakcuDevice.cs` (exchanges). Code comments link to the sections here and do not repeat the facts.

## Packet

```text
DE AD | LEN:u16 LE | CMD:u8 | PAYLOAD[LEN]
```

- `LEN` is the payload length. It does not count `CMD`.
- A request with no payload has `LEN = 0`.
- All multi-byte values are little endian.
- A `GET` that MAKCU accepts gets a reply of the same shape. The result bytes are the payload.
- A `SET` that MAKCU accepts gets no reply. `INTERPOLATE` is the exception: its reply echoes the value.

## Reject form

A reject of a `MOVE` or of any `GET` is:

```text
DE AD 01 00 | CMD | FF
```

There is no success status byte.

## Opcodes

| Opcode | Name               | Operation | Request payload                 | Reply payload                          | Used by the client |
| ------ | ------------------ | --------- | ------------------------------- | -------------------------------------- | ------------------ |
| `0x02` | `DEVICE`           | GET       | empty                           | `kinds:u8`, a bit mask: `0x01` mouse   | yes                |
| `0x04` | `FIRMWARE_VERSION` | GET       | empty                           | `version:u32`                          | no                 |
| `0x18` | `MOVE`             | SET       | `x:i16 y:i16`                   | none                                   | yes                |
| `0x1F` | `INTERPOLATE`      | SET       | `spread:u8`                     | `spread:u8` (the echo)                 | yes                |
| `0xA5` | `SET_BAUD`         | SET       | see [Baud switch](#baud-switch) | none                                   | yes                |

`INTERPOLATE` also has a `GET` form (empty payload). The client does not use it.

## Connect order

The client finds the device by its serial port and then runs this sequence (`MakcuDevice.ConnectPort`):

1. Open the port at 4 000 000 baud and send `DEVICE`. The device's rate after the app last closed is unknown, so this is tried first.
2. If there is no reply, open the port at 115 200 baud and send `DEVICE`. If that also gets no reply, the port fails.
3. At 115 200 baud, send `SET_BAUD` 4 000 000, switch the port to 4 000 000 baud, and send `DEVICE` again.
4. A `DEVICE` reply without the mouse bit (`0x01`) makes the port unusable for the client.

## Firmware version

The firmware version is `major * 1000 + build`. V4.085 is `4085`. V4.094 is `4094`. It is written as `V{major}.{build:03}`.

The client does not query the firmware version.

## Byte examples

| Request                 | Bytes                         |
| ----------------------- | ----------------------------- |
| `DEVICE`                | `DE AD 00 00 02`              |
| `FIRMWARE_VERSION`      | `DE AD 00 00 04`              |
| `MOVE` x = 10, y = -5   | `DE AD 04 00 18 0A 00 FB FF`  |
| `INTERPOLATE` 255 (Auto)| `DE AD 01 00 1F FF`           |
| `INTERPOLATE` 40        | `DE AD 01 00 1F 28`           |
| `SET_BAUD` 4000000      | `DE AD 05 00 A5 00 09 3D 00`  |
| `SET_BAUD` 115200       | `DE AD 05 00 A5 00 C2 01 00`  |

The reply to `FIRMWARE_VERSION` is `DE AD 04 00 04 <version:u32>`.

## Move interpolation

The `INTERPOLATE` value is a `u8`.

- `0..=100`: a fixed percent.
- `255`: Auto. This is the default since V4.041.
- `101..=254`: MAKCU rejects them.

Auto reads back as `0xFF`. A reject of `INTERPOLATE` is `DE AD 01 00 1F FF`, which is the same bytes as the echo of Auto. The client cannot tell a reject of Auto from the echo, so it treats any reply to Auto as confirmation. A reply to a fixed percent must echo that percent.

## Baud switch

```text
DE AD 05 00 A5 <baud:u32>
```

For this one packet, the length `5` counts the command byte. This is different from the rule in [Packet](#packet). The MAKCU pages (see [Sources](#sources)) give the packet in this form. From V4.039, the firmware reads the fixed payload and ignores `LEN`. Before V4.039 the packet is not usable.

- MAKCU accepts 115200 and 4000000. From V4.073, `0` also selects 115200.
- The switch is not saved. After a restart, MAKCU starts at its saved rate.
- MAKCU sends no reply. The client reopens the port at the new rate and probes with `DEVICE`.

## Sources

Read on 2026-10-11:

- <https://www.makcu.com/en/api>
- <https://www.makcu.com/api/versions>
