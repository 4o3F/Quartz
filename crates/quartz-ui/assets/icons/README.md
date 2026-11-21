# Fluent 图标

SVG 来自 [microsoft/fluentui-system-icons](https://github.com/microsoft/fluentui-system-icons/tree/6c2c9075cf8952ef48369b2475233b6e9761efed)，固定提交 `6c2c9075cf8952ef48369b2475233b6e9761efed`，许可证为 [MIT](LICENSE)。[manifest.json](manifest.json) 列出每个本地文件对应的上游路径、图标名、显示尺寸与样式；SVG 内容保持上游原样。

常规图标使用 Regular，侧边栏当前页使用 Filled。标准显示尺寸为 20 px，紧凑尺寸为 16 px。上游 `Calendar Week Start` 和 `Window Console` 缺少 16 px 版本，紧凑显示使用对应的 20 px SVG 缩放到 16 px。

`AppAssets` 通过 `include_bytes!` 嵌入文件，只注册这些 Fluent 资源。组件使用的 `icons/*.svg` 路径也解析到对应 Fluent 资源。测试遍历源码中的 `IconName` 引用，并检查全部注册路径与侧边栏 Filled 资源；缺失路径返回错误。

错误界面使用 Warning 和 Dismiss Circle 的 28 px Regular 图标，Copy 使用常规尺寸。`ProgressRing.svg` 的环形来自已批准的 [错误界面稿](../../../../docs/design/mockups/errors.html)，由主题着色并通过 GPUI 的 Spinner 旋转；该本地图形单独列于清单的 `custom_icons`。
