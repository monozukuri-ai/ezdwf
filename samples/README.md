# DWF/DWFx/XPS sample corpus

サンプルの出所、期待ハッシュ、観測済み構造は
[`manifest.json`](./manifest.json) に記録する。第三者ファイルのライセンスを
このプロジェクトのライセンスと混同しないため、ダウンロード済みファイルは
`samples/external/` に置き、Git 管理外とする。

## 取得

リポジトリ直下で次を実行する。

```console
python scripts/fetch_samples.py
```

ネットワークを使わず、既に取得済みのファイルだけを検証する場合:

```console
python scripts/fetch_samples.py --verify-only
```

取得対象と保存先は `manifest.json` を正とする。スクリプトはサイズとSHA-256の
両方が台帳と一致しない限り、ファイルを確定しない。sample IDを引数にすると
必要なファイルだけを取得できる。

## 現在のコーパス

`autodesk-blocks-and-tables` は Autodesk が配布している DWF 6.00 の
サンプルで、Imperial/Metric の 2 枚の ePlot シートを含む。コンテナ、
manifest、section descriptor、埋め込みフォント、thumbnail、binary/ASCII
混在 W2D、markup を一度に検証できるため、最初の統合テストに適している。

ただし配布ページだけでは完全な DWF ファイルの再配布条件を確認できなかった。
現段階では開発者が配布元から各自取得する `local-download-only` とし、fixture
としてコミットしない。ZIP/DWF/W2D の小さな回帰 fixture は
`tests/python/conftest.py` で仕様に基づいて実行時生成し、第三者バイナリをテスト配布物へ
混入させない。

`ecma-388-openxps`はECMAがECMA-388標準ページから公式配布するXPS版仕様書である。
1,202 ZIP entries、1 document、494 FixedPageを持ち、fixed payload XMLはUTF-16LE、
visualは202,852件、clipを持つvisualは38,143件（最大chain深さ2）である。optional
integration testは全ページを読み、diagnostic 0件までを固定する。これはDWFxと
共有するOPC/XPS parserの大規模互換性fixtureだが、Autodesk製DWFxそのものではない。
ECMA配布ファイルについてもprojectからの再配布条件までは確定していないため、binaryは
commitせず`local-download-only`とする。

DWFxについては、content types、root/part relationships、FixedDocumentSequence、
FixedDocument、FixedPage、画像、font partを持つ最小OPC packageを同じ場所で生成する。
Path/Glyphs、static resource、Canvas transform/clip、segment単位のpaint、画像brush、
UTF-16 XML、外部relationship非取得、path traversal/DTD/上限をCIで検証する。

実ファイルとして、Design Review 2009で作成された`mydrawing2009.dwfx`と空のA4 DWFxも
検証する。前者はPath/Text、markup、packaged fontとglyph outline解決を含む。これらは
古いDesign Review由来のため、現行Autodesk製品のDWFx出力互換性を証明するものではない。

CAD Forum由来では、次のファイルを追加している。

- `xanadu-test.dwf`: DWF Composer 2のmarkupを含むDWF 6.00
- `mydrawing2009.dwfx`: Design Review 2009の描画・markup・packaged font
- `empty-sheet.dwfx` / `empty.dwf`: 空ページと空packageの境界ケース
- `forest-map.dwf`: 5,788本のpolylineを持つ地図
- `layout-d-size.dwf`: 複数layerと8,618 entityを持つD-size図面
- `guardrail-detail-v0030.dwf` / `hvac-drawing-v0050.dwf`: 形式認識はできるが
  decode未対応のstandalone legacy DWF

CAD Forumカタログの利用条件はコンテンツの再配布を制限している。そのため、これらの
バイナリは必ず配布元からローカル取得し、Git、wheel、sdistへ含めてはならない。

## 追加すべきカバレッジ

現在のコーパスでも、次の経路は検証できない。

- standalone DWF/W2D 00.30と00.50のdecode、および別versionのlegacy stream
- ASCII-only W2D と、opcode ごとの最小 fixture
- compressed data、画像、Unicode text、URL、複数 layer/viewport
- malformed ZIP/XML/W2D と、展開量・nesting 上限
- 現行Autodesk製品が生成した実DWFxとのinterop比較
- password/encryption、signature、restricted content

第三者ファイルを追加する場合は、URL だけでなく取得日、ハッシュ、正しい
magic/header、実ファイルに適用されるライセンスを確認してから台帳へ追加する。
