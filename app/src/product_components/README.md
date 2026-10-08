# Product component adapter

One runtime component/theme dependency: the fixed Ely CE core profile. `product_components`
is a host adapter for the established RemotePlay product appearance, not a second framework.
Buttons use the actual Ely Button; theme state lives only in Ely Theme, and the root uses
Ely FocusScope. Product Theme/ThemeSet are semantic value snapshots/configuration, with no
Global implementation or independent manager. Original palette, typography, compact dimensions,
icons and tooltip layout are preserved through the adapter.

## Retained Yororen code and assets

The scalar text input, grapheme/selection model, editing actions, theme value shapes and ten
original SVGs derive from [Yororen UI 0.2.0](https://github.com/MeowLynxSea/yororen-ui/tree/31005a92286067b56dcc45e7833b91e428533274),
commit `31005a92286067b56dcc45e7833b91e428533274`, Copyright 2026 MeowLynxSea.
`INPUT-UPSTREAM.json` preserves upstream paths and hashes. The fixed upstream LICENSE and
NOTICE are copied verbatim, including the GPUI/Zed attribution; modified Rust files carry notices.
These source-derived adapters remain Apache-2.0 software even though the runtime Yororen crate
has been removed. No claim is made that these are newly authored Ely input implementations.

Changes: local imports/action macro path, product style adapter, one Ely-backed theme manager,
and provenance notices. Cursor blinking remains the upstream 500 ms interval. The editing,
UTF-16, IME and grapheme machinery is retained to avoid dropping product input behavior during
the first component slice. Native IME/selection acceptance and replacing this adapter with an
Ely input are subsequent work, not proven by compilation. Original ten SVG bytes are unchanged;
new Ely assets keep their separate upstream licenses.
