# Own the MAKCU client

The app has its own MAKCU client in `src/makcu`. It speaks only the binary `DE AD` protocol over a Windows COM port, and it needs MAKCU firmware V4.076 or newer. The vendored SDK `vendor/makxd` is removed, and the features that the app does not use (KM text commands, encryption, UDP, BLE, keyboard, controller) are not carried over. The decision keeps one protocol and one firmware floor, so the app has no second Injection API.

Considered options:

- Keep the vendored SDK. Rejected: it is about 6,700 lines for a few commands, and it was made for firmware V30.
- Keep KM_API as a second Injection API. Rejected: the binary protocol with firmware V4.076 covers everything the app does.
- Accept firmware V30 as well. Rejected: it keeps the old branches that this decision removes.
