# nice-aimassist

Rust + egui で構成した、ローカル完結の最小構成です。

残している機能:

- `models/` からのローカル YOLO26 ONNX モデル読み込み
- DXGI Desktop Duplication による画面中央キャプチャ
- ONNX Runtime + DirectML 推論
- 公式 MAK Rust SDK を使ったマウス / コントローラー出力
- 1機能に複数割り当て可能なグローバルホットキー
- 整数スケールの単一 `Sensitivity`
- Continuous推論の整数FPS上限
- 単発推論による手動キャリブレーション
- capture / preprocess / inference / postprocess / output / total の性能計測

意図的に含めていないもの:

- Kalman / prediction / tracker / wind mouse
- depth inference / TensorRT / CUDA 独自経路
- WinRT / virtual camera / UDP capture
- 追加の入力バックエンド
- overlay / debug drawing / data collection
- 複数の感度パラメータや smoothing / randomization
- 独自 NMS

## 実行ディレクトリ

モデルと設定は実行ファイル基準です。

```text
nice-aimassist.exe
settings.json
models/
  yolo26n.onnx
  yolo26s.onnx
```

`models/` は起動時または再スキャン時に必要なら自動作成されます。モデル選択は OS のファイルダイアログを使わず、`models/` 内の `.onnx` だけを一覧表示します。設定には絶対パスではなくモデルのファイル名だけを保存します。

設定形式の後方互換は持ちません。現在の `settings.json` は `version = 2` のみを受け付け、古い形式はデフォルト設定として扱います。

## UI の流れ

通常画面は次の順序です。

1. **Devices** — MAK本体、Mouse route、Controller routeの種類、物理Controllerの確認結果を別々に表示します。
2. **Model** — `models/` 内から選択します。ロード成功時だけ設定へcommitします。
3. **Output** — Mouse / Controller のユーザー選択を保持し、routeの一時消失で勝手に切り替えません。
4. **Calibration** — 整数Sensitivityを調整し、`Run once` で単発確認します。
5. **Continuous** — 複数のHold shortcutのどれかが押されている間だけ動作します。FPS上限もここで設定します。
6. **Hotkeys** — `+ Add` から `Waiting for input…` に入り、次のキー入力を登録します。
7. **Diagnostics** — capture / MAK / detection / performance の詳細を必要時だけ表示します。

`Ready` はモデル、DXGI、MAK接続、選択Output routeが利用可能なときに表示されます。

## Devices 表示

MAKのroute情報と物理Controllerの接続確認は同じ意味ではないため、UIで分離しています。

```text
MAK                 ● Connected · COM4
Mouse route         ● Ready
Controller route    ● Xbox GIP
Physical controller ● Connected
```

MAKのphysical snapshotはfirmwareや一時的なsample状態によって取得できない場合があります。その場合は `Unavailable / not confirmed` と表示し、`Disconnected` と断定しません。接続直後は一度確認し、Controller出力を選択しているIdle中のみ5秒間隔で更新します。Mouse出力中は同期クエリを繰り返さず、必要なら `Check` で手動確認できます。

通常の抜線復帰はMAK SDKのauto-reconnectを利用します。`Refresh routes` は route変更後の明示的な再接続で、Controller zero release → `Device::disconnect()` → teardown待ち → 同一COM優先 → 必要時だけ再スキャン、の順で実行します。

## Model

モデルは `models/` からのみ選択します。

- `Refresh` で一覧を再読み込み
- `Open models folder` でExplorerから配置先を開く
- 新モデルのロードに失敗した場合、以前の正常なモデルを維持
- モデルが1個だけで設定が空なら自動ロード

## Sensitivity

Sensitivityは小数ではなく整数のpercent scaleです。

```text
100% = 1×
50%  = 0.5×
200% = 2×
```

内部では `gain = sensitivity / 100` としてだけ変換します。UIは直接整数入力と `-10 / -1 / +1 / +10` の微調整を提供します。

## FPS limit

Continuous推論だけに整数FPS上限を適用します。既定値は240 FPS、設定範囲は1〜1000 FPSです。`Run once`には適用しません。

各フレームのcapture開始時刻を基準にフレーム周期を計算し、処理が上限周期より早く終わった場合だけ残り時間を待ちます。待機中もworker commandを受信するため、Hold shortcutを離したときの停止、Sensitivity変更、FPS上限変更などを単純な`sleep`でブロックしません。処理自体が上限周期を超えた場合は追加待機を行わず、実FPSは処理性能に従います。

## Hotkeys

各actionには0個以上のshortcutを割り当てられます。

```text
Hold continuous   [F8 ×] [Ctrl+F8 ×] [+ Add]
Run once          [F9 ×]             [+ Add]
```

`+ Add` を押すと入力待ちになります。

```text
Waiting for input…
Press a key or key combination
Esc to cancel
```

登録待ち中は既存global hotkeyのdispatchを一時停止します。`+ Add` をキーボード操作で開いたときの Enter / Space を誤登録しないよう、captureは次のUI passから開始します。重複shortcutは別actionを含め禁止です。複数のHold shortcutが同時に押された場合は、最後の1個が離されるまでContinuousを停止しません。Windows/Super modifierはcapture UIでは割り当てません。

## 性能計測

最初の推論はcold runとしてrolling統計から分離します。以降は直近256 sampleを保持します。

- Capture: DXGI取得 + GPU→CPU readback
- Preprocess: center crop + BGRA→RGB + normalize + NCHW
- Inference: ONNX Runtime `Session::run()`
- Postprocess: output parse + nearest target selection
- Output submit: MAK SDK 呼び出し
- Pipeline: capture開始からoutput submitまで
- Cadence: continuous run開始間隔

通常画面ではFPSとPipeline / Inferenceのp50・p95だけを出し、詳細はDiagnosticsへ置きます。

## 構造

```text
src/
  main.rs        eframe起動
  app.rs         UI compositionとUI状態
  config.rs      Config V2 / Sensitivity / hotkey binding
  paths.rs       exe基準のmodels/settings path
  models.rs      models/ scanとExplorer起動
  hotkeys.rs     global hotkey登録・複数binding・入力capture
  engine.rs      worker state machine
  capture.rs     DXGI Desktop Duplication
  detector.rs    ONNX Runtime / YOLO26
  mak_output.rs  MAK lifecycle / route / physical controller status
  metrics.rs     rolling performance metrics
vendor/makxd/    MAK Suite Rust SDK
```

DXGI / ONNX Session / MAK Device は単一worker threadが所有します。UIはcommandを送り、status snapshotを読むだけです。

## ビルド / 検証

Windows + MSVC Rust toolchainを使います。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

VS Codeでは `cargo: verify` task が format check → clippy → test を順番に実行します。

## VS Code debugging

VS Codeでは `rust-analyzer` と `CodeLLDB` を推奨拡張として登録しています。`Run and Debug` から次を選べます。

```text
nice-aimassist: Debug
nice-aimassist: Debug tests
```

`nice-aimassist: Debug` はCargoのdebug buildをCodeLLDBで起動します。ブレークポイント、ステップ実行、変数、call stackを確認できます。`nice-aimassist: Debug tests` はbinary targetのテスト実行ファイルをデバッガで起動します。

F5デバッグ時は `NICE_AIMASSIST_DATA_DIR=${workspaceFolder}` を設定するため、debug executableが `target/debug/` にあっても、モデルと設定はワークスペース直下を使います。

```text
nice-aimassist/
  models/
  settings.json
  src/
  target/
```

通常起動で `NICE_AIMASSIST_DATA_DIR` が未設定の場合は、従来どおり実行ファイルのディレクトリを使用します。デバッグ設定では `RUST_BACKTRACE=1` も有効です。

性能計測はdebug buildの値を基準にせず、`cargo run --release` またはrelease buildで確認してください。
