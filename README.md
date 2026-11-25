# SumiPDF

Windows x64 用の小さな PDF ビューアーです。文書に黄色のハイライトを付け、Windows の個人証明書で電子署名できます。アプリ本体は Rust 製の単一 EXE です。

## 使い方

1. **[最新リリース](https://github.com/t11ii/SumiPDF/releases/latest)** から Windows x64 用の ZIP をダウンロードし、展開して `SumiPDF.exe` を起動します。対象は Windows 11 と Windows 10 バージョン 1709 以降の x64 環境です。
2. PDF をウィンドウにドロップするか、**開く…** を押します。エクスプローラーの「プログラムから開く」や `SumiPDF.exe "C:\Documents\契約書.pdf"` でも開けます。

### ハイライトを付ける

1. **蛍光ペン** を押し、ページ上の文字を選び始める位置から、選び終わる位置までドラッグします。複数行をまたぐと、選んだ文字に沿って行ごとに黄色が付きます。
2. スキャン画像や文字の位置を取得できない部分は、塗りたい範囲の対角をドラッグして囲みます。余白からドラッグした場合も矩形で指定できます。**元に戻す** で直前に付けたハイライトを取り消せます。
3. **注釈を保存…** でコピーを保存します。たとえば `契約書.pdf` の既定の保存名は `契約書-highlighted.pdf` です。保存後も、開いている文書で編集を続けられます。タイトルの `*` は未保存の変更を示します。

保存形式は PDF 標準の Highlight 注釈です。文字位置を使う選択は横書きの一般的な欧文フォントや日本語の Identity-H フォントに対応します。縦書きフォントや特殊な文字配置では、矩形で指定できます。既に電子署名された PDF と暗号化 PDF はハイライトの編集対象外です。

### 電子署名する

1. **電子署名…** を押して証明書を選びます。Windows の「現在のユーザー → 個人」に、秘密鍵を使える有効な文書署名用証明書が必要です。組織から発行された PFX を使う場合は、あらかじめ Windows の証明書インポートウィザードで「現在のユーザー」に登録します。
2. 保存先を選びます。既定のファイル名は `契約書-signed.pdf` です。画面で付けたハイライトも署名対象に含まれます。署名を検証してから新規ファイルとして保存します。
3. 署名者を確認するときは、署名済み PDF を Acrobat Reader などの署名対応ビューアーで開き、署名パネルを表示します。署名者の信頼判定は、受け手が信頼する証明書と検証環境によって決まります。

## 閲覧操作

| 操作 | キー・マウス |
| --- | --- |
| ファイルを開く | Ctrl + O |
| 前後のページ | ← / →、Page Up / Page Down、全体表示時のホイール |
| 最初・最後のページ | Home / End |
| 指定ページに移動 | ページ番号をクリック、または Ctrl + G → 番号 → Enter |
| 拡大・縮小 | Ctrl + ホイール、Ctrl + プラス / マイナス |
| ページ全体を表示 | Ctrl + 0 |
| 拡大中の表示位置を移動 | ドラッグ、ホイール、Shift + ホイールで横移動 |
| 蛍光ペンの切り替え | H（終了は Esc でも可） |
| 今回付けたハイライトを一つ取り消す | Ctrl + Z |
| ハイライト付きコピーを保存 | Ctrl + Shift + S |
| 電子署名 | Ctrl + S |

## 軽量化と署名の仕様

- 閲覧には Windows 標準の PDF エンジンを使います。元ファイルをストリームで開き、表示ページだけをバックグラウンドで描画します。ページ移動・リサイズ中に古くなった描画要求を省きます。表示用ビットマップの上限は 1 枚あたり 32 MiB です。プロセス全体の使用量は文書や表示サイズによって変わります。[Microsoft の PDF 描画 API](https://learn.microsoft.com/en-us/uwp/api/windows.data.pdf.pdfdocument?view=winrt-19041)
- 蛍光ペンを使うときだけ、描画とは別のスレッドで PDF を解析し、表示ページの文字位置を求めます。ツールを閉じると、解析データを解放します。新しいハイライトは原文の末尾に注釈として追記します。
- 署名用の全文解析は署名操作時に行います。表示に使ったファイルストリームから原文を取り出し、PDF の末尾に署名フィールドと署名辞書を追記します。元ファイルは閲覧中、読み取り共有で開きます。大きな PDF に署名する際は、元データ・解析結果・出力に応じて追加のメモリーを使います。
- Windows の暗号プロバイダーが秘密鍵で SHA-256 の分離 CMS 署名を作成します。PDF の署名形式は `adbe.pkcs7.detached` です。署名日時には PC の時計を使います。認定タイムスタンプや長期検証情報の付与は未対応です。[Microsoft の署名 API](https://learn.microsoft.com/en-us/windows/win32/api/wincrypt/nf-wincrypt-cryptsignmessage)
- 署名対象は未署名・非暗号化の PDF です。既存の電子署名や証明による制限、XFA フォームがある文書、相互参照の修復を要する文書、署名後のサイズが 4 GiB 以上になる文書は対象外です。パスワードを入力して文書を開く機能はありません。

## ビルドと検証

Windows x64 の Rust stable と Visual C++ Build Tools でビルドできます。

```powershell
cargo test --locked
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release --locked --target x86_64-pc-windows-msvc
./tests/smoke.ps1 -Executable ./target/x86_64-pc-windows-msvc/release/SumiPDF.exe
```

Windows 用のテストは一時証明書を使う署名、改ざん検出、実際の PDF のレンダリングを扱います。スモークテストではリリース版で 2 ページの PDF を開き、初回表示とページ切り替えを確認し、経過時間とワーキングセットを記録します。同じ検査を実行する [Windows CI](.github/workflows/windows.yml) も付属しています。

macOS からは Rust の `x86_64-pc-windows-gnu` ターゲットと MinGW-w64 の GCC・windres を使います。

```sh
cargo build --release --locked --target x86_64-pc-windows-gnu
```

OpenSSL、NSS の certutil、Poppler の pdfsig、pdftotext、pdftoppm が使える環境では、以下で外部検証テストも実行できます。複数行の文字を選んで注釈を保存し、該当箇所の画素が黄色に変わることや、回転・トリミングに対する位置を確認します。署名については改ざん検出と署名前後のページ画像を検査します。

```sh
cargo test --locked -- --include-ignored
```
