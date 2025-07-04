<br/>
<p align="center">
  <img src="https://github.com/shinkuan/RandomStuff/assets/35415788/db94b436-c3d4-4c57-893e-8db2074d2d22" width="50%">
  <h1 align="center">Akagi</h1>

  <p align="center">
「死ねば助かるのに………」- 赤木しげる<br>
<br>
    <br/>
    <br/>
    <a href="https://discord.gg/Z2wjXUK8bN">有問題請至 Discord 詢問</a>
    <br/>
    <br/>
    <a href="./README.md">English</a>
    <br/>
    <a href="https://github.com/shinkuan/Akagi/issues">回報錯誤</a>
    .
    <a href="https://github.com/shinkuan/Akagi/issues">功能請求</a>
  </p>
</p>

<p align="center">
  <a href="https://github.com/shinkuan/Akagi"><img src="https://img.shields.io/github/stars/shinkuan/Akagi?logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/shinkuan/Akagi/releases"><img src="https://img.shields.io/github/v/release/shinkuan/Akagi?label=release&logo=github" alt="GitHub release" /></a>
  <a href="https://github.com/shinkuan/Akagi/issues"><img src="https://img.shields.io/github/issues/shinkuan/Akagi?logo=github" alt="GitHub issues" /></a>
  <a href="https://github.com/shinkuan/Akagi"><img src="https://img.shields.io/github/languages/top/shinkuan/Akagi?logo=python" alt="Top language" /></a>
  <a href="https://discord.gg/Z2wjXUK8bN"><img src="https://img.shields.io/discord/1192792431364673577?label=discord&logo=discord&color=7289DA" alt="Discord" /></a>
  <a href="https://deepwiki.com/shinkuan/Akagi"><img src="https://deepwiki.com/badge.svg" alt="Ask DeepWiki"></a>
</p>

# Playwright


這個Branch利用 **Playwright** 提供使用者更簡便且更穩定流暢的體驗。此branch的一個主要優勢是無需複雜的代理設定；**不再需要設定 mitmproxy 或 Proxifier 等工具**而且**內建自動打牌**功能，**允許自動化任務在背景中運行**。

TL;DR
- **無需 mitmproxy 或 Proxifier**。
- **內建自動播放**。
- **可在背景運行**。

# 關於

## 「本專案旨在提供一個便利的方式，讓玩家可以即時了解自己在麻將對局中的表現，並藉此學習與進步。此專案僅供教育用途，作者不對使用者利用此專案採取的任何行為負責。若使用者違反遊戲服務條款，遊戲開發者與發行商有權進行處置，包含帳號停權等後果，與作者無關。」

![image](./docs/images/example_majsoul.png)

# 功能
- 即時顯示對局資訊
- 即時顯示 AI 評估
- 支援雀魂、天鳳、麻雀一番街、天月
- 支援四人麻將與三人麻將
- 可使用多種 AI 模型
  - 內建模型
  - 線上伺服器模型
  - 自製模型
- 自動化對局
  - *只有在Windows Release版中開啟ot_server時可用
- TUI 介面
  - 支援多種主題

# 目錄

### ⚠️ 仔細閱讀以下內容再開始使用 ⚠️
### ⚠️ 仔細閱讀以下內容再開始使用 ⚠️
### ⚠️ 仔細閱讀以下內容再開始使用 ⚠️

- [關於](#關於)
- [開始前](#開始前)
- [安裝](#安裝)
- [使用方式](#使用方式)
- [常見問題](#常見問題)
- [開發者](#開發者)
- [授權](#授權)

# 開始前

> [!WARNING]  
> 此分支不需要任何代理工具，例如 mitmproxy 或 Proxifier。
> 下方影片為主分支內容。

🎥[教學影片連結](https://youtu.be/Z88Ncxbe2nw)

### 你將需要：

1. 一個 `mjai_bot`
   1. 此項目已經包含一個可用的`mortal` mjai bot在[這邊](./mjai_bot/mortal)
      - 由於檔案大小限制，在`./mjai_bot/mortal`下的`mortal.pth`是一個較小的模型
      - 不建議在實際對局中使用
      - 若想取得其他模型，可以從 [Discord](https://discord.gg/Z2wjXUK8bN) 取得
      - 若想取用更強的 AI 模型，也可以使用線上伺服器架設的模型
      - 可從 [Discord](https://discord.gg/Z2wjXUK8bN) 取得 API 金鑰
   2. 或自行製作，請參閱 [開發者](#開發者)
2. 使用 Windows Terminal 開啟 Akagi 才能看到漂亮的 TUI

> [!WARNING]  
> 請使用 Windows Terminal，否則介面顯示將異常

### 支援的麻將遊戲：

**此分支僅支援雀魂。**
**其他遊戲請使用主分支。**

| 平台          | 四人麻將       | 三人麻將       | 自動打牌 |
| ------------- | ------------- | -------------- | -------- |
| __雀魂__      | &check;       | &check;        |  &check; |
| __天鳳__      | &cross;       | &cross;        |  &cross; |
| __麻雀一番街__ | &cross;       | &cross;        |  &cross; |
| __天月__      | &cross;       | &cross;        |  &cross; |


# 安裝

- 一般使用者：
  - 前往 [release 頁面](https://github.com/shinkuan/Akagi/releases)
  - 下載最新Playwright版
  - 解壓縮 ZIP 檔案
  - (Optional) 將 MJAI 機器人放到 `./Akagi/mjai_bot`
  - 執行 `run_akagi.exe`
  - (Optional) 在設定中輸入線上伺服器 API 金鑰
- 開發者：
  - clone 此專案
  - 使用 Python 3.12
  - 安裝相依套件 `pip install -r requirements.txt`
  - 使用 `python -m playwright install` 安裝 Playwright 瀏覽器
  - 將 MJAI 機器人放到 `./Akagi/mjai_bot`
    - 內建模型：將 `./mjai_bot/mortal/libriichi/libriichi-<version>-<platform>.<extension>` 移動到 `./mjai_bot/mortal/libriichi.<extension>`。
    - 三人模型亦同。
  - 執行 `run_akagi.py`

# 使用方式


1. **檢查設定與 AI 模型**
   1. 選擇模型
      - 點選左下角的「Model」按鈕
      - 從清單中選擇一個模型
      - 若沒有模型，可從 [Discord](https://discord.gg/Z2wjXUK8bN) 取得
      - 內建預設模型為弱 AI
      - __3 人對局請選 3P 模型！__
      - __不要用 4P 模型參與 3 人對局！__
   2. 檢查設定
      - 點選左下角的「Settings」按鈕
      - 檢查設定是否正確
      - 若你有取得線上伺服器 API 金鑰，請在設定中輸入
      - 線上伺服器提供更強的 AI 模型
      - 可從 [Discord](https://discord.gg/Z2wjXUK8bN) 取得 API 金鑰
   3. 儲存設定
      - 點選「Save」按鈕
      - 將設定儲存下來
   4. 重新啟動 Akagi
      - 關閉 Akagi 並重新開啟
      - 設定才會套用

4. **啟動遊戲用戶端**
5. **加入對局**
6. **檢查 Akagi**
   1. 現在你應該能看到 AI 實時分析對局
   2. 若沒有，請檢查設定與 Proxifier 設定
   3. 或是檢查Logs，看看是否有錯誤訊息
   4. 若有錯誤訊息，可以到 [Discord](https://discord.gg/Z2wjXUK8bN) 尋求協助

## 操作說明

### Akagi

#### 啟動 MITM 代理伺服器：
> [!IMPORTANT]
> 下圖的 MITM 文字是主分支的內容，如果你用這個分支你會看到Playwright。

![image](./docs/gifs/start_mitm.gif)

#### 選擇 AI 模型：
模型儲存在 `./mjai_bot/` 資料夾
![image](./docs/gifs/select_model.gif)

#### 變更設定：
> [!IMPORTANT]  
> 重啟後設定才會套用

![image](./docs/gifs/settings.gif)

#### 開啟日誌：
出現問題時可開啟日誌以了解狀況，並附上回報
儲存路徑：`./logs/`
![image](./docs/gifs/logs_screen.gif)

#### 切換 MJAI 資訊視窗內容：
點選該視窗即可切換
![image](./docs/gifs/change_window.gif)

#### AutoPlay:
確保遊戲用戶端的顯示比例設定為 16:9
![image](./docs/images/autoplay_example/good.png)
![image](./docs/images/autoplay_example/bad_1.png)
![image](./docs/images/autoplay_example/bad_2.png)

#### 更換主題：
![image](./docs/gifs/change_theme.gif)

#### 立直宣告
由於MJAI協議的限制，建議欄為立直時並不會顯示棄牌。
你必須手動點擊立直按鈕來宣告立直。
![image](./docs/gifs/call_reach.gif)

<!-- Trobleshooting -->
# 常見問題

你可以到[Discord](https://discord.gg/Z2wjXUK8bN)上詢問問題。

> [!TIP]
> 若你有任何問題，請附上日誌檔案，這樣我才能更快地幫助你。
> 日誌檔案位於 `./logs/` 資料夾中。

> [!NOTE]
> 若你在使用過程中遇到任何問題，你可以到Github Issues頁面回報問題。
> 或是到[Discord](https://discord.gg/Z2wjXUK8bN)上詢問問題。

# 開發者

## 專案結構

- `./Akagi/` - 專案主要資料夾
  - `akagi/` - Akagi 的 Textual UI
  - `logs/` - 日誌
  - `playwright_client/` - Playwright 用戶端
    - `bridge/` - 遊戲用戶端與伺服器的橋接器，用來轉換為 MJAI 協議
      - `majsoul/` - 雀魂橋接器
  - `mjai_bot/` - MJAI 機器人
    - `base/` - 機器人基礎類別，可參考自製機器人
    - `*/` - 各種 bot，例如 `mortal/`
  - `settings/` - 設定檔資料夾
  - `run_akagi.py` - 執行主程式

## 橋接器

> [!WARNING] 
> 不建議在此分支上建立自己的橋接器。
> 此分支僅設計用於雀魂。
> 如果你想建立自己的橋接器，請使用主分支。

## MJAI 機器人

要製作 MJAI 機器人，需實作 `mjai_bot/base/bot.py` 中的 `Bot` 類別。

TODO: 製作一個 tsumogiri bot 範例

## TODO

- [x] 支援三人麻將
- [x] 支援 RiichiCity
- [x] 立直後推薦切牌
- [ ] 槓後推薦切牌（極少見）

# 作者

- [Shinkuan](https://github.com/shinkuan/) - shinkuan318@gmail.com
- [Discord](https://discord.gg/Z2wjXUK8bN)


# 授權條款

```
“Commons Clause” License Condition v1.0

The Software is provided to you by the Licensor under the License, as defined below, subject to the following condition.

Without limiting other conditions in the License, the grant of rights under the License will not include, and the License does not grant to you, the right to Sell the Software.

For purposes of the foregoing, “Sell” means practicing any or all of the rights granted to you under the License to provide to third parties, for a fee or other consideration (including without limitation fees for hosting or consulting/ support services related to the Software), a product or service whose value derives, entirely or substantially, from the functionality of the Software. Any license notice or attribution required by the License must also include this Commons Clause License Condition notice.

Software: Akagi

License: GNU Affero General Public License version 3 with Commons Clause

Licensor: shinkuan
```
