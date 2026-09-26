# nice-aimassist

Windows 専用の Rust + egui アプリです。DXGI Desktop Duplication で画面中央をキャプチャし、YOLO ONNX モデルを ONNX Runtime + DirectML で推論して、MAK SDK 経由でマウス / コントローラーへ出力します。

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
- `settings.json` は `version = 2` のみ対応です。古い形式はデフォルト設定になります。

## 設定項目

- **Sensitivity**: 整数の percent です。`gain = sensitivity / 100`。
- **Target Y offset**: ターゲット高さに対する補正です（-100〜100%、既定 0%）。Sensitivity 適用後に加算されます。
- **Confidence**: 検出を採用する最小値です（0〜100%、既定 50%）。
- **FPS limit**: Continuous のみに適用します（1〜1000、既定 240）。`Run once` には適用しません。
- **Low-latency mode**: DirectML の完了待ちをスピン待ちにします。最悪遅延が下がる代わりに、動作中は CPU コアを 1 つ使い切ります。切り替えるとモデルを再ロードします。
- **Output**: Mouse / Controller をユーザーが選択します。route が消えても自動では切り替えません。
- **Hotkeys**: 1 つのアクションに複数のショートカットを割り当てられます。Continuous は Hold shortcut のどれかが押されている間動作します。重複は不可です。

## 構造

- `src/app/`: UI
- `src/engine/`: capture worker と inference worker を調整する worker。出力（検出→MAK 送信）は inference thread 上で行います
- `src/platform.rs`: スレッド優先度・MMCSS・電力スロットリング解除
- `src/graphics/`: DXGI capture、GPU preprocess、共有フレームプール
- `src/inference/`: ORT DirectML 推論と postprocess
- `src/mak.rs`: MAK の接続・route 管理
- `vendor/makxd/`: MAK SDK

フレームは CPU に readback せず、GPU 上で inference に渡します。

## ビルド / 検証

Windows + MSVC の Rust toolchain を使います。性能は release build で確認してください。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```
