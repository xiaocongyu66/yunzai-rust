# 渲染回归用例（tests/render_cases）

自研 HTML/CSS 渲染器的**离线回归资产**：14 个自包含 HTML（内联 CSS、无外链图片、统一 `body { width: 600px; margin: 0; padding: 0; }`），每个用例只验证 1 个特性，并给出可直接人工/PIL 核对的几何预期（画布尺寸 + 块所在象限 + 采样点颜色）。

配套脚本 `run_tests.sh` 只消费已构建好的二进制，**不含任何编译命令**。

## 运行方式

```bash
bash tests/render_cases/run_tests.sh <yunzai二进制路径>
# 例：bash tests/render_cases/run_tests.sh ./bin/yunzai
```

- 逐个执行 `<yunzai> --render-test <用例>.html /tmp/render_test_out/<用例>.png 600`
- 校验：退出码 0 且 PNG > 500 字节（崩溃、空白图、写盘失败都会 FAIL）
- 全部通过 → exit 0；任一失败 → exit 1（失败用例列表 + 各用例日志 `/tmp/render_test_out/<用例>.log`）
- 环境变量：`RENDER_TEST_WIDTH`（渲染视口宽，默认 600）、`RENDER_TEST_OUT`（输出目录）

## 用例清单与预期视觉

| 用例 | 验证点 | 预期视觉（600 宽画布） | 关键采样点 |
| --- | --- | --- | --- |
| t01_flex_column | flex 纵向排列 | 600x300；红/绿/蓝三条 600x100 横带无缝相接 | (300,50)红 (300,150)绿 (300,250)蓝 |
| t02_absolute_inset | absolute 四向 inset | 灰 200x200 容器；红块 (10,10)-(180,170) 即 170x140 | (100,5)灰 (190,100)灰 (100,100)红 (181,171)灰 |
| t03_gradient | 180deg 线性渐变 | 600x150 整幅竖直红→蓝渐变，同行同色 | 列 x=300：(251,0,4)→(127,0,128)→(3,0,252) |
| t04_border | 四侧异色 6px 边框 | 白底 200x100 块，上红/右蓝/下绿/左黄 6px 框，内容区 188x88 | (100,3)红 (197,50)蓝 (100,97)绿 (3,50)黄 (100,50)白 |
| t05_box_shadow | 外阴影 + inset 阴影 | A 蓝 120x80 @(40,40)，右/下 10px 处 35% 黑阴影带（rgb≈166 灰）；B 红 120x80 @(320,40)，内边缘 8px 加深环，中心纯红 | (165,100)灰 (324,80)暗红 (380,80)红 |
| t06_text_shadow | 文字阴影 (2,2) | 600x80；40px 蓝字，深灰蓝阴影同形偏移右下 2px，y>60 纯白 | 文字区内找阴影像素 (x,y)，则 (x-2,y-2) 附近为蓝 |
| t07_opacity_zindex | z-index 排序 + opacity | 600x200；半透明红(z2)盖住纯蓝(z1)：重叠区紫 ≈(128,0,128)，A 独占区蓝，B 独占区粉 ≈(255,128,128) | (30,60)蓝 (140,60)紫 (140,160)粉 |
| t08_pseudo | ::before / ::after 注入位置 | 灰 200x160 纵向容器：B 字块(红底) → 绿 200x50 条 → 黄 200x50 条 → A 字块(蓝底)；B 在绿上、A 在黄下 | (100,50)绿 (100,100)黄；B/A 字形分别在 y 0..30 与 y 120..160、x 0..30 内 |
| t09_transform | rotate+scale+translate 复合 | 红块 200x100 @(100,50) 顺时针倾 15°、放大 1.2、平移后中心约 (232,121)；四角 ≈(131,32)(363,94)(332,210)(100,148) | (200,100)红 (110,60)白（原角让出）(320,190)红（新覆盖） |
| t10_calc | min-width: calc() | 紫块 400x80（width 100px 被 min-width=600-200=400 抬高） | (399,40)紫 (401,40)白 |
| t11_nowrap_ellipsis | nowrap/ellipsis/裁剪 | 画布 600x40；200x40 容器内长文本，容器外（x>200 或 y>40）无文字像素 | 容器右、下边缘外均为白 |
| t12_border_radius4 | border-radius 多值 | 红块 200x200，60px 圆角切削四角（引擎取首值应用于四角） | (5,5)白 (60,60)红 (100,2)红 (195,195)白 |
| t13_multi_bg | 多层背景叠加 | 600x150；上层 60% 半透明蓝 + 下层红→黄：水平方向多色相渐变，两层数据均可见 | y=75 行 x=30/300/570 三点两两不同色 |
| t14_overflow_clip | overflow:hidden 裁剪 | 灰容器 300x200；红块 200x200 @(200,100) 越界部分被裁，红仅出现在 (200..300,100..200) | (250,150)红 (350,150)白 (150,150)灰 |

## PIL 快速核对示例

```bash
yunzai --render-test tests/render_cases/t07_opacity_zindex.html /tmp/render_test_out/t07.png 600
python3 - <<'EOF'
from PIL import Image
im = Image.open('/tmp/render_test_out/t07.png').convert('RGB')
assert im.size == (600, 200), im.size
print('A独占(蓝)', im.getpixel((30, 60)))     # (0,0,255)
print('重叠(紫)', im.getpixel((140, 60)))     # ≈(128,0,128)
print('B独占(粉)', im.getpixel((140, 160)))   # ≈(255,128,128)
EOF
```

## 基线要求与已知版本差异

用例 CSS 全部落在渲染器已支持范围内，但个别断言依赖绘制层新近能力，基线差异如下（差异即回归信号，升级后应逐条转绿）：

- **t04（四侧异色）**：需绘制层支持 `border-width/border-color` 四值展开与逐侧绘制。基线版本退化为整体单色（黑色回退）——先核对边框宽度恒为 6px。
- **t14 / t11（overflow 裁剪）**：需绘制层 overflow:hidden 子树裁剪。基线版本越界内容照画（t14 的 (350,150) 为红、t11 文字溢出到 x>200）。
- **t13（多层背景）**：目标为逐层绘制（第一层最上）；基线版本把两层合并为单渐变（水平方向出现多段色标过渡）。共同断言"两层数据均可见"在两种实现下都成立。
- **t12（圆角多值）**：当前引擎取首值 60px 应用于四角；四角独立为已知缺口。核对四角切削半径恒为 60px。
- **t11（nowrap/ellipsis）**：canary 用例——`white-space:nowrap`、`text-overflow:ellipsis` 尚未被引擎消费，核心断言以 overflow 裁剪为准。

## CI 接入

本目录只做"渲染 → 校验产物"，不编译。接入现有构建流水线（参考 `.github/workflows/build.yml` 的 yunzai 构建产物），在拿到二进制的步骤之后追加：

```yaml
      - name: 渲染回归（render cases）
        run: bash tests/render_cases/run_tests.sh ./bin/yunzai
        # 需要人工/像素级比对时，上传渲染产物
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: render-cases-png
          path: /tmp/render_test_out/
```

也可在本地对已有二进制做冒烟：`bash tests/render_cases/run_tests.sh bin/yunzai`，逐张查看 `/tmp/render_test_out/*.png` 与上表比对。
