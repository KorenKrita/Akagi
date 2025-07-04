# Akagi

加入 [Discord 社群](https://discord.gg/Z2wjXUK8bN) 以獲得支援與更新資訊。

邀請連結: https://discord.gg/Z2wjXUK8bN

### YouTube 教學
[安裝與設定Akagi](https://youtu.be/Z88Ncxbe2nw)
https://youtu.be/Z88Ncxbe2nw

## 操作指南

1. **安裝 Visual C++ Redistributables**
   1. 首先，確保你使用的是管理員權限的 PowerShell。找到 PowerShell，右鍵點擊快捷方式並選擇 `以管理員身份執行`
   2. 複製並粘貼以下命令到 PowerShell 並按 Enter:
      > Set-ExecutionPolicy Bypass -Scope Process -Force; [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; iex ((New-Object System.Net.WebClient).DownloadString('https://vcredist.com/install.ps1'))
   3. 這將下載並安裝最新的 Visual C++ Redistributables。
   4. 安裝完成後，重新啟動電腦。
   5. 以上教學來自 [vcredist.com](https://vcredist.com/quick/)

2. **檢查設定與 AI 模型**
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
   5. 啟動Playwright

5. **啟動遊戲用戶端**
6. **加入對局**
7. **檢查 Akagi**
   1. 現在你應該能看到 AI 實時分析對局
   2. 若沒有，請檢查設定與 Proxifier 設定
   3. 或是檢查Logs，看看是否有錯誤訊息
   4. 若有錯誤訊息，可以到 [Discord](https://discord.gg/Z2wjXUK8bN) 尋求協助

## 疑難排解

開發中。
請至 [Discord](https://discord.gg/Z2wjXUK8bN) 尋求協助。
