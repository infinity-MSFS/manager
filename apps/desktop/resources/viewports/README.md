# FLIR nose camera DLLs

`cargo xtask dist` copies `infinity_viewports_bridge.dll` and
`infinity_viewports_core.dll` here from a release build of
[infinity-viewports](https://github.com/infinity-MSFS/infinity-viewports)
(`INFINITY_VIEWPORTS_DIR`, else `../infinity-viewports`). The bundle installs
this folder as `viewports/` beside the manager, the first place the FLIR host
looks. The DLLs are build outputs and are not committed.
