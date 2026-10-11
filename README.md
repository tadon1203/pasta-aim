# nice-aimassist

Windows 専用の Rust + egui アプリです。DXGI Desktop Duplication で画面中央をキャプチャし、YOLO ONNX モデルを ONNX Runtime + DirectML で推論して、MAK SDK 経由でマウスへ出力します。

## 実行ディレクトリ

`settings.json`・`models/`・`logs/` は実行ファイルと同じ場所に置かれます。環境変数 `NICE_AIMASSIST_DATA_DIR` を設定するとその場所を使います。

```text
nice-aimassist.exe
settings.json
models/
  yolo26n.onnx
```

- モデルは `models/` 内の `.onnx` をファイル名で選択します。ロードに失敗した場合は以前のモデルを維持します。
- 入出力が FP32 / FP16 のどちらのモデルにも対応しています。精度はモデルから自動判別し、UI の `Active input` に表示します。
- モデルは入力サイズが固定で、出力が `[1, N, 6]`（x1, y1, x2, y2, score, class）の end-to-end 形式（NMS 埋め込み）である必要があります。動的 shape や end2end でないモデルは、ロード時に理由付きで拒否されます。
- `settings.json` に version はありません。項目が欠けている場合はその項目だけ既定値になります。未知の項目や範囲外の値がある場合は、ファイル全体が既定値になります。

## 設定項目

- **Sensitivity**: 整数の percent です。`gain = sensitivity / 100`。
- **Target Y offset**: ターゲット高さに対する補正です（-100〜100%、既定 0%）。Sensitivity 適用後に加算されます。
- **Confidence**: 検出を採用する最小値です（0〜100%、既定 50%）。
- **FPS limit**: Continuous の取得開始間隔に適用します（1〜1000、既定 240）。新規画像と再利用画像で共通です。処理が遅れてもまとめて実行せず、`Run once` には適用しません。
- **New frame wait**: DXGI の取得1回あたりの最大待ち時間です（1〜20ms、既定 2ms）。FPS limit とは独立しています。
- **When no new frame**: 新規の画面更新がない場合は、直前の画像を GPU 上で再利用します（既定 `Reuse previous frame`）。`Wait for new frame` では推論・送信をスキップします。キャプチャ異常では再利用しません。
- **Maximum reuse age**: 元の画面更新時刻から測る再利用期限です（1〜50ms、既定 15ms）。再利用で期限は延長されません。推論開始前と出力直前にも確認し、期限切れの再利用画像からは送信しません。`Run once` は新規画像のみを使います。
- **Low-latency mode**: DirectML の完了待ちをスピン待ちにします。最悪遅延が下がる代わりに、動作中は CPU コアを 1 つ使い切ります。切り替えるとモデルを再ロードします。
- **Overlay**: 検出したターゲットの box をモデル入力領域（画面中央）に重ねて表示します（既定 OFF）。線幅（1〜8px）、色、不透明度、アウトライン幅（0〜4px、0 で無し）、アウトライン色を変更できます。ボーダーレス / ウィンドウ化フルスクリーンで動作し、排他フルスクリーンでは表示されません。オーバーレイ自体は画面キャプチャから除外されるため、モデルに映り込みません。
- **Output**: マウス出力のみです（コントローラー出力は廃止）。ホットキーには XInput コントローラーを使えます。
- **Hotkeys**: 1 つのアクションに複数のショートカットを割り当てられます。Continuous は Hold shortcut のどれかが押されている間動作します。重複は不可です。

## 構造

- `src/app/`: UI
- `src/engine/`: エンジンスレッドで取得→GPU コピー・前処理→推論・後処理→MAK 出力を直列に実行します。出力が完了してから次の取得を開始します
- `src/platform.rs`: スレッド優先度・MMCSS・電力スロットリング解除
- `src/graphics/`: DXGI capture、GPU preprocess、D3D11/D3D12 間で共有する1枚のフレームテクスチャと完了フェンス
- `src/inference/`: ORT DirectML 推論と postprocess
- `src/overlay/`: box オーバーレイ（専用スレッド、DirectComposition + Direct2D）
- `src/mak.rs`: MAK の接続・route 管理
- `vendor/makxd/`: MAK SDK

フレームは CPU に readback せず、GPU 上で inference に渡します。

停止・終了・モデル／画面変更のコマンドは取得前と推論完了後・出力前に確認します。取得と推論の実行中は完了を待つため、停止応答にはその処理時間が含まれます。FPS 待機中はコマンドを受信できます。画面切替は候補の準備が完了してから反映し、失敗時は旧構成を保持して連続実行を再開します。モデル切替は成功・失敗とも停止状態になります。オーバーレイ描画と診断トレースの書き込みには専用スレッドを使います。

## ビルド / 検証

Windows + MSVC の Rust toolchain を使います。性能は release build で確認してください。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```
