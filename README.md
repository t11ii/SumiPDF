# SumiPDF

Windows x64 用の小さな PDF ビューアーです。4色の蛍光ペンと下線で文書に注釈を付け、Windows の個人証明書で電子署名できます。アプリ本体は Rust 製の単一 EXE です。

## 使い方

1. **[最新リリース](https://github.com/t11ii/SumiPDF/releases/latest)** から Windows x64 用の ZIP をダウンロードし、展開して `SumiPDF.exe` を起動します。対象は Windows 11 と Windows 10 バージョン 1709 以降の x64 環境です。
2. PDF をウィンドウにドロップするか、**開く…** を押します。エクスプローラーの「プログラムから開く」や `SumiPDF.exe "C:\Documents\契約書.pdf"` でも開けます。

### 蛍光ペンと下線を使う

1. **蛍光ペン** または **下線** を押し、隣の **色** で黄・赤・緑・青から選びます。最初の色は黄色です。選んだ色は、これから付ける注釈に適用されます。
2. 文字の先頭から末尾までドラッグします。複数行を選ぶと、行ごとに色や下線が付きます。スキャン画像や余白では、蛍光ペンは塗りたい範囲の対角を、下線は線を引きたい位置で横にドラッグします。**元に戻す** で直前の注釈を取り消せます。
3. **注釈を保存…** でコピーを保存します。たとえば `契約書.pdf` の既定の保存名は `契約書-highlighted.pdf` です。保存後も、開いている文書で編集を続けられます。タイトルの `*` は未保存の変更を示します。

蛍光ペンは PDF 標準の Highlight 注釈、下線は Underline 注釈として保存します。文字位置を使う選択は横書きの一般的な欧文フォントや日本語の Identity-H フォントに対応します。縦書きフォントや特殊な文字配置では、画像と同じ操作で位置を指定できます。既に電子署名された PDF と暗号化 PDF は注釈の編集対象外です。

### 電子署名する

1. **電子署名…** を押し、署名位置の一覧で記入先を選びます。PDF に空の署名フィールドがあれば、ページ番号と名前が表示されます。印刷された署名欄やスキャンには **署名位置をドラッグして指定** を選び、欄を囲みます。青枠が署名の表示位置です。従来の不可視署名も選べます。
2. **透かし** で標準模様または自分の PNG を選びます。**透かし画像を作成…** で同梱のデザイナーが開きます。文字・書体・外枠・色・手描きを組み合わせ、**PNG を保存** したら、署名画面の **PNG 画像を選択…** で読み込みます。編集用の SVG も書き出せます。[透かしデザイナー](dist/watermarkDesigner.html)
3. **署名して保存…** を押し、Windows の「現在のユーザー → 個人」から署名用証明書を選んで新しいファイルに保存します。証明書の氏名を左に、署名者情報と UTC 日時を右に配置し、透かしを背景に重ねます。画面で付けた注釈と署名の外観を含む PDF 全体が署名対象です。[参考にした Acrobat の署名表示](https://www.adobe.com/devnet-docs/acrobatetk/tools/DigSigDC/appearances.html)
4. 証明書や署名後の変更は、Acrobat Reader などの署名パネルで確認できます。信頼判定は受け手の証明書と検証環境によって決まります。

### Claude Code・Codex から署名する

同じ Windows EXE をコマンドとして呼び出せます。`--inspect` で署名フィールドを列挙し、`--preview` でページや候補位置を PNG で確認し、`--sign` で署名済みコピーを作成します。印刷された欄やスキャンを扱うときは、エージェントがページ画像を読み、該当する矩形を指定します。使い方と PowerShell の実行例は ZIP にも同梱しています。[エージェント用ガイド](dist/signingGuide.md)

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
| 下線の切り替え | U（終了は Esc でも可） |
| 今回付けた注釈を一つ取り消す | Ctrl + Z |
| 注釈付きコピーを保存 | Ctrl + Shift + S |
| 電子署名 | Ctrl + S |

## 軽量化と署名の仕様

- 閲覧には Windows 標準の PDF エンジンを使います。元ファイルをストリームで開き、表示ページだけをバックグラウンドで描画します。ページ移動・リサイズ中に古くなった描画要求を省きます。表示用ビットマップの上限は 1 枚あたり 32 MiB です。プロセス全体の使用量は文書や表示サイズによって変わります。[Microsoft の PDF 描画 API](https://learn.microsoft.com/en-us/uwp/api/windows.data.pdf.pdfdocument?view=winrt-19041)
- 蛍光ペンか下線を使うときだけ、描画とは別のスレッドで PDF を解析し、表示ページの文字位置を求めます。ツールを閉じると、解析データを解放します。新しい注釈は原文の末尾に追記します。
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

Windows 用のテストは一時証明書を使う署名、改ざん検出、実際の PDF のレンダリングを扱います。PowerShell 7 のスモークテストは、2 ページの PDF で起動・ページ送り・署名位置の選択を検査します。CLI テストは現在のユーザーの個人ストアに一時的なテスト証明書を作り、PNG を指定した署名と原本のハッシュを検査した後、証明書とテスト鍵を削除します。同じ検査を実行する [Windows CI](.github/workflows/windows.yml) も付属しています。

macOS からは Rust の `x86_64-pc-windows-gnu` ターゲットと MinGW-w64 の GCC・windres を使います。

```sh
cargo build --release --locked --target x86_64-pc-windows-gnu
```

OpenSSL、NSS の certutil、Poppler の pdfsig、pdftotext、pdftoppm が使える環境では、以下で外部検証テストも実行できます。保存した PDF を画像に描画し、蛍光ペンと下線の4色、複数行の選択範囲、回転・トリミング後の位置を確認します。署名については改ざん検出と署名前後のページ画像を検査します。

```sh
cargo test --locked -- --include-ignored
```
