# MAKCU protocol

The part of the MAKCU binary protocol that the client in `src/makcu` uses. The code is in `src/makcu/protocol.rs`. Code comments link to the sections here and do not repeat the facts.

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

| Opcode | Name             | Operation | Request payload                           | Reply payload                                  |
| ------ | ---------------- | --------- | ----------------------------------------- | ---------------------------------------------- |
| `0x02` | `DEVICE`         | GET       | empty                                     | `kinds:u8`, a bit mask: `0x01` mouse           |
| `0x04` | `FIRMWARE_VERSION` | GET     | empty                                     | `version:u32`                                  |
| `0x18` | `MOVE`           | SET       | `x:i16 y:i16`                             | none                                           |
| `0x1F` | `INTERPOLATE`    | SET       | `spread:u8`                               | `spread:u8` (the echo)                         |
| `0x69` | `RAW_MOVE`       | SET       | `id:u32 x:i16 y:i16 timeout_uf:u16`       | 6 bytes, see [Raw move](#raw-move)             |
| `0x6A` | `RAW_STATUS`     | query     | `id:u32 consume:u8`                       | 6 or 8 bytes, see [Raw move](#raw-move)        |
| `0xA5` | `SET_BAUD`       | SET       | see [Baud switch](#baud-switch)           | none                                           |

`INTERPOLATE` also has a `GET` form (empty payload). The client does not use it.

## Firmware version

The firmware version is `major * 1000 + build`. V4.085 is `4085`. V4.094 is `4094`. It is written as `V{major}.{build:03}`.

The client needs V4.085 or newer (`4085`). This is the oldest official release that has the Raw move commands (they exist from V4.074).

## Byte examples

| Request                                           | Bytes                                       |
| ------------------------------------------------- | ------------------------------------------- |
| `DEVICE`                                          | `DE AD 00 00 02`                            |
| `FIRMWARE_VERSION`                                | `DE AD 00 00 04`                            |
| `MOVE` x = 10, y = -5                             | `DE AD 04 00 18 0A 00 FB FF`                |
| `INTERPOLATE` 255 (Auto)                          | `DE AD 01 00 1F FF`                         |
| `INTERPOLATE` 40                                  | `DE AD 01 00 1F 28`                         |
| `RAW_MOVE` id = 1, x = 100, y = 0, timeout = 50   | `DE AD 0A 00 69 01 00 00 00 64 00 00 00 32 00` |
| `RAW_MOVE` id = 1, x = 5, y = -5, timeout = 800   | `DE AD 0A 00 69 01 00 00 00 05 00 FB FF 20 03` |
| `RAW_STATUS` id = 1, consume = false              | `DE AD 05 00 6A 01 00 00 00 00`             |
| `RAW_STATUS` id = 1, consume = true               | `DE AD 05 00 6A 01 00 00 00 01`             |
| `RAW_STATUS` id = 0 (metadata)                    | `DE AD 05 00 6A 00 00 00 00 00`             |
| `SET_BAUD` 4000000                                | `DE AD 05 00 A5 00 09 3D 00`                |
| `SET_BAUD` 115200                                 | `DE AD 05 00 A5 00 C2 01 00`                |

The reply to `FIRMWARE_VERSION` is `DE AD 04 00 04 <version:u32>`.

## Move interpolation

The `INTERPOLATE` value is a `u8`.

- `0..=100`: a fixed percent.
- `255`: Auto. This is the default since V4.041.
- `101..=254`: MAKCU rejects them.

Auto reads back as `0xFF`. A reject of `INTERPOLATE` is `DE AD 01 00 1F FF`, which is the same bytes as the echo of Auto. The client cannot tell a reject of Auto from the echo.

## Baud switch

```text
DE AD 05 00 A5 <baud:u32>
```

For this one packet, the length `5` counts the command byte. This is different from the rule in [Packet](#packet). The MAKCU pages (see [Sources](#sources)) give the packet in this form. From V4.039, the firmware reads the fixed payload and ignores `LEN`. Before V4.039 the packet is not usable.

- MAKCU accepts 115200 and 4000000. From V4.073, `0` also selects 115200.
- The switch is not saved. After a restart, MAKCU starts at its saved rate.
- MAKCU sends no reply. The client reopens the port at the new rate and probes.

## Raw move

`RAW_MOVE` (`0x69`) sends the full X and Y displacement as one USB report. MAKCU never splits it and never cuts it. A displacement that does not fit one report ends as a rejected record. The result of a request is read later with `RAW_STATUS` (`0x6A`).

### Request ids

- An id is a `u32`. It is not zero. It is strictly greater than every id that MAKCU has accepted since it started.
- An id that is consumed cannot be used again.
- All callers share one id sequence.
- A serial reconnect does not reset the ids or the records. A device restart does.
- When the ids run out, only a device restart gives a new sequence.
- MAKCU holds at most 16 records (live or not yet consumed).

### `timeout_uf`

- The unit is 125 µs. It is a timing unit only. It does not mean that MAKCU sends 8 kHz.
- `0` means 8000 (1000 ms). The valid range is `1..=16383` (at most 2047.875 ms).
- The timeout starts when the request reaches the head of the queue. It includes the wait for older Auto movement and USB retries. It does not include the time that the request waits behind older requests. It is not a limit on the total latency.

### Reply payload of `RAW_MOVE` and of `RAW_STATUS` with an id

6 bytes:

| Offset | Field   | Type |
| ------ | ------- | ---- |
| 0      | `state` | u8   |
| 1      | `reason`| u8   |
| 2..6   | `id`    | u32  |

The reply packet has the same `CMD` as the request.

| State | Meaning                                                                    |
| ----- | -------------------------------------------------------------------------- |
| 1     | Queued. `reason` is 0. It does not mean sent.                              |
| 2     | Submitted to the endpoint. `reason` is 0.                                  |
| 3     | Rejected. A final state. `reason` says why.                                |
| 255   | Immediate error. MAKCU did not accept the request and made no record.      |

Reasons of state 3:

| Reason | Meaning                                    |
| ------ | ------------------------------------------ |
| 17     | Range or descriptor check failed           |
| 18     | Timed out                                  |
| 20     | The target was lost before the submit      |
| 21     | Endpoint reset                             |

Other binding or configuration reasons can also appear.

Reasons of state 255:

| Reason | Meaning                                                         |
| ------ | --------------------------------------------------------------- |
| 1      | The length, id, consume flag, or timeout is not valid           |
| 2      | The id is not above the highest accepted id                     |
| 3      | The 16 records or the command queue have no room                |
| 4      | The mouse target or a needed field is not available             |
| 5      | The id is not known, or its result was already consumed         |
| 6      | A consume of a request that is still queued. The record stays   |
| 7      | A firmware update or a restart is in progress                   |

An error makes no record and does not move `last_accepted_id`.

### `RAW_STATUS`

- `consume = 0` reads the record and keeps it. It can be repeated.
- `consume = 1` is only valid for a final record (state 2 or 3). It returns the result and frees the record. For a queued record it returns state 255, reason 6.
- Read and keep a final result with `consume = 0` before the consume. If the consume reply is lost, the kept result is still there.
- Id `0` is the metadata query. `consume` must be 0.

Metadata reply (8 bytes):

| Offset | Field              | Type | Meaning                  |
| ------ | ------------------ | ---- | ------------------------ |
| 0      | `0`                | u8   | always 0                 |
| 1      | `schema`           | u8   | always 1                 |
| 2..6   | `last_accepted_id` | u32  | the newest accepted id   |
| 6      | `free_records`     | u8   | free records             |
| 7      | `capacity`         | u8   | always 16                |

Examples of reply payloads:

| Payload                    | Meaning                                  |
| -------------------------- | ---------------------------------------- |
| `01 00 01 00 00 00`        | id 1 queued                              |
| `FF 03 02 00 00 00`        | id 2 not accepted, reason 3 (no room)    |
| `03 12 07 00 00 00`        | id 7 rejected, reason 18 (timed out)     |
| `00 01 01 00 00 00 10 10`  | metadata: last id 1, 16 free, capacity 16 |

### Lost replies

If the reply of a submit is lost, query the original id. Do not send the same displacement again with a new id. After a device restart, read the metadata again. An old id does not prove the session is the same.

## Sources

Read on 2026-10-11:

- <https://www.makcu.com/en/api>
- <https://www.makcu.com/api/versions>
- <https://www.makcu.com/api/raw-v4074.md>
