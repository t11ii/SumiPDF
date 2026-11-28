# SumiPDF を Claude Code・Codex から使う

Windows 上のエージェントがローカルの `SumiPDF.exe` をコマンドとして呼び出します。署名には、Windows の「現在のユーザー → 個人」または「ローカル コンピューター → 個人」にある証明書、またはローカルの `.pfx` / `.p12` ファイルに含まれる有効な署名用証明書を使えます。署名するアカウントには、その秘密鍵を使う権限が必要です。エージェントは標準出力の JSON、終了コード、プレビューで書き出した PNG を使います。非暗号化の PDF に初回署名と追加署名ができます。

**依頼例**　「SumiPDF を使って、契約書.pdf の承認者欄を探して。ページと位置を見せて。署名時の透かしには私の seal.png を使って」

## 署名欄を選ぶ

1. `--inspect` でページ数、入力ファイルの SHA-256、記入可能な署名フィールドの一覧を取得します。各欄には ID、フィールド名、ページ番号、表示上の矩形が入ります。`alreadySigned` が true なら既存署名があります。`allowsNewField` が false のときは、一覧にある許可済みの欄を `--field` で指定してください。追加する人は、直前に署名された PDF を入力にして改めて位置とハッシュを確認します。
2. 署名者の役割をフィールド名と周囲の記載に照らして選びます。`--preview` が書き出した PNG を画像として開くと、選んだ欄の位置と前後の文言を確認できます。作成者と承認者のどちらで署名するかが決まらない場合は、依頼者に候補と該当ページを示します。
3. スキャンや印刷された罫線だけの PDF では、まず `--preview` と `--page` でページ画像を取得します。エージェントが画像上のラベルと余白を読み、署名先を矩形で指定します。矩形の4値は、回転と切り抜きを反映した画像の左・上・右・下です。左上が `0,0`、右下が `1,1` です。PNG の寸法が 1200 × 1600 で、左上 (600, 1200)、右下 (1080, 1360) を選んだ場合は `0.5,0.75,0.9,0.85` になります。

## PowerShell での呼び出し例

```powershell
$exe = '.\SumiPDF.exe'
$pdf = '.\契約書.pdf'
$info = & $exe --inspect $pdf | ConvertFrom-Json
$info.fields | Format-Table id,name,tooltip,page,rect

# 一覧にある欄のうち、今回の役割に対応する ID を指定
$id = Read-Host '署名欄 ID'
& $exe --preview $pdf --field $id --out '.\署名位置.png' | Out-Host
& $exe --sign $pdf --field $id --watermark '.\seal.png' --expect-sha256 $info.sha256 --out '.\契約書-署名済.pdf' | Out-Host
```

印刷された署名欄に配置する場合は、例えば次のようにページと範囲を指定します。プレビューと署名に同じ矩形を渡します。

```powershell
& $exe --preview $pdf --page 2 --out '.\page2.png' | Out-Host
$rect = '0.5,0.75,0.9,0.85'
& $exe --preview $pdf --page 2 --rect $rect --out '.\選択位置.png' | Out-Host
& $exe --sign $pdf --page 2 --rect $rect --expect-sha256 $info.sha256 --out '.\契約書-署名済.pdf' | Out-Host
```

- `--signature-image '.\signature.jpg'` で、可視署名の左半分に PNG または JPEG を配置できます。右半分には証明書の氏名と、行を分けた **Date** と **Time**（UTC）を表示します。
- `--watermark` を省略すると、SumiPDF の標準の透かしを使用します。同梱の `watermarkDesigner.html` では、手描きや文字を組み合わせて透過 PNG を作れます。
- PFX を使うときは `--sign` に `--pfx "C:\署名\自分の証明書.pfx"` を付けます。パスワードは本人が画面で入力します。署名用証明書が複数あれば選択画面が開きます。Windows の個人ストアへの事前登録は不要です。
- `--certificate` と `--pfx` を両方省略すると 証明書の氏名と Windows ストアの格納先を一覧で確認できます。指定した証明書で処理する場合は `--certificates` の JSON で氏名（`name`）、拇印（`thumbprint`）、格納先（`store`）を確認し、`--certificate <thumbprint>` を付けます。IC カード等の鍵は PIN 入力を要求することがあります。
- `--expect-sha256` は、確認したファイルと署名時のファイルの一致を検査します。対象が変わっていたら、新しい PDF で署名欄を再確認します。
- 出力先には新しいパスを指定します。成功時は標準出力に JSON が返ります。失敗時は標準エラーに JSON が返り、終了コードは 1 です。GUI 形式の EXE のため、PowerShell の例ではパイプで出力を受けています。エージェントから子プロセスとして呼ぶときは標準出力・標準エラーを取得し、終了を待ちます。
- 電子署名の検証には Acrobat Reader などの署名パネルを使えます。署名の外観には証明書の氏名と UTC 日時が入り、秘密鍵の処理は Windows の鍵プロバイダーが担当します。

## 画面で署名するときの初期設定

署名ツールで最後に選んだ透かしと署名画像のパス、証明書を Windows ユーザーごとに記憶します。PFX のパスワードも省略する場合は、入力時に **Remember password on this PC** を選びます。Windows の DPAPI で暗号化したデータをローカルに保存します。**Alt + Space → Forget saved signing certificate** で、証明書の選択と記憶したパスワードを消去できます。

Acrobat DC 用のカスタム透かし `%APPDATA%\Adobe\Acrobat\DC\Security\SignatureLogo.pdf` を設定済みなら、初回から背景に使います。別の場所にある1ページの PDF は **Choose Acrobat watermark PDF…** で選択できます。コマンドラインで署名する場合は、上記の `--certificate`、`--pfx`、`--watermark` で署名ごとに指定します。
