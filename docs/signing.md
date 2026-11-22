# 代码签名

- [Windows：Azure Artifact Signing](#windows-azure-artifact-signing)
- [macOS：签名与公证](#macos-签名与公证)

## Windows：Azure Artifact Signing

Windows 11 的“智能应用控制”（Smart App Control）会直接拦截没有可信签名、微软云端
也不认识的程序，SmartScreen 也会弹“已保护你的电脑”。发布流程在下面 3 个 Secrets 和
3 个 Variables 都有时，会用 Azure Artifact Signing（原名 Trusted Signing）给 Windows
构建签名并加时间戳；缺任何一个就照常打出未签名的安装包，发布不会失败，任务摘要里会
写明“built unsigned”，发布说明里保留“未签名”的提示。

签名的文件：`anixops-ssh.exe`（守护进程 `easysshd` 是同一个可执行文件带 `--easysshd`
启动，所以一起签了）、NSIS 安装包、安装包里的卸载程序和 NSIS 插件。这些由 Tauri 的
`bundle.windows.signCommand` 调用 [`scripts/windows-sign.ps1`](../scripts/windows-sign.ps1)
完成；签名配置只在 CI 里通过 `tauri build --config` 传入，本地构建不受影响。构建后
`windows-sign.ps1 verify` 检查 exe、安装包和安装包里的 exe 都是有效签名且带时间戳
（Artifact Signing 的证书只有 3 天有效期，没有时间戳的签名几天后就失效）。

CI 在每次打安装包时用一次性的自签名测试证书走同一条签名流程，所以签名脚本或 Tauri
升级出问题会在 PR 上就失败，而不是等到发版那天。

**更新器不受影响**：应用内更新读取的 `anixops-ssh-windows.appcast.json` 是在签名、上传
之后由 `update-feed` 任务从 Release 上下载安装包再计算大小、SHA-256 和签名的，校验的
就是签过名的那个文件。

费用：Basic 套餐约 9.99 美元/月（每月 5000 次签名，一次发版约 3–5 次）。签名后的程序
立即被智能应用控制认可，不需要像 OV 证书那样慢慢积累 SmartScreen 信誉。

### 1. 开通 Artifact Signing

1. 有一个 Azure 订阅（<https://portal.azure.com>）。
2. 在订阅的“资源提供程序”里注册 `Microsoft.CodeSigning`。
3. 新建 **Artifact Signing Account**，选一个区域（例如 West Europe），记下账号名和
   区域对应的端点，例如 `https://weu.codesigning.azure.net`（端点必须和账号区域一致，
   否则签名时报 403）。
4. 在账号里给自己分配 **Artifact Signing Identity Verifier** 角色，然后在
   “Identity validation” 提交身份验证（组织需要营业执照等资料；个人开发者能否申请
   以 Azure 当时的地区政策为准）。审核通过后才能建证书配置。
5. 新建 **Certificate profile**，类型选 **Public Trust**，关联通过的身份验证，记下
   配置名。

### 2. 创建给 CI 用的服务主体

1. Microsoft Entra ID → 应用注册 → 新注册（例如 `anixops-ssh-release-signing`），
   记下 **目录（租户）ID** 和 **应用程序（客户端）ID**。
2. 证书和密码 → 新客户端密码，复制“值”（只显示一次）。到期前要换新并更新 Secret。
3. 回到 Artifact Signing 账号（或只在那个证书配置上）→ 访问控制 (IAM) → 添加角色分配：
   **Artifact Signing Certificate Profile Signer**，成员选上面的应用。

### 3. 填入 GitHub Secrets 和 Variables

仓库 Settings → Secrets and variables → Actions：

| 类型 | 名称 | 内容 |
|---|---|---|
| Secret | `AZURE_TENANT_ID` | 租户 ID |
| Secret | `AZURE_CLIENT_ID` | 应用程序（客户端）ID |
| Secret | `AZURE_CLIENT_SECRET` | 客户端密码的值 |
| Variable | `ARTIFACT_SIGNING_ENDPOINT` | 例如 `https://weu.codesigning.azure.net` |
| Variable | `ARTIFACT_SIGNING_ACCOUNT` | Artifact Signing 账号名 |
| Variable | `ARTIFACT_SIGNING_PROFILE` | 证书配置名 |

```bash
gh secret set AZURE_TENANT_ID --repo AnixOps/AnixOps-ssh
gh secret set AZURE_CLIENT_ID --repo AnixOps/AnixOps-ssh
gh secret set AZURE_CLIENT_SECRET --repo AnixOps/AnixOps-ssh
gh variable set ARTIFACT_SIGNING_ENDPOINT --repo AnixOps/AnixOps-ssh --body https://weu.codesigning.azure.net
gh variable set ARTIFACT_SIGNING_ACCOUNT --repo AnixOps/AnixOps-ssh --body <账号名>
gh variable set ARTIFACT_SIGNING_PROFILE --repo AnixOps/AnixOps-ssh --body <证书配置名>
```

### 4. 发布

之后每个版本的 Windows 包都会签名；发布说明自动去掉“未签名”的提示。验证：下载
安装包 → 右键 → 属性 → 数字签名，能看到签名者和时间戳。

### 其他办法（不需要 Azure）

- **SignPath Foundation**：给开源项目免费签名，需要提交项目审核，签名者显示为
  “SignPath Foundation”。接入时把 `windows-sign.ps1` 的 `sign` 换成 SignPath 的
  提交流程即可，其余（验证、发布说明）不变。
- **OV/EV 证书**（硬件令牌或云 HSM）：每年几百美元起；OV 证书要积累一段时间的
  下载量才会被 SmartScreen 信任。
- **Microsoft Store 上架 MSIX**：商店替你签名，但需要额外的 MSIX 打包和商店审核。

## macOS：签名与公证

发布流程（`.github/workflows/release.yml`）在仓库里有下面 6 个 Secrets 时，会自动给
macOS 安装包签名并提交 Apple 公证；没有时照常打出未签名的安装包。证书和密钥只放在
GitHub Secrets 里，不进仓库。

### 1. 加入 Apple Developer Program

在 <https://developer.apple.com/programs/> 以个人或组织身份加入（99 美元/年）。

### 2. 创建 Developer ID Application 证书

在 Mac 上打开 Xcode → Settings → Accounts → 选中团队 → Manage Certificates →
左下角 + → **Developer ID Application**。然后在“钥匙串访问”里找到这张证书，
右键导出为 `.p12`，设置一个导出密码。

把 `.p12` 转成一行 base64：

```bash
base64 -i DeveloperID.p12 -o DeveloperID.b64
```

证书的完整名称（形如 `Developer ID Application: Your Name (TEAMID1234)`）可以这样查：

```bash
security find-identity -v -p codesigning
```

### 3. 创建 App Store Connect API 密钥（用于公证）

<https://appstoreconnect.apple.com/access/integrations/api> → Team Keys →
生成一个角色为 **Developer** 的密钥，下载 `AuthKey_XXXXXXXXXX.p8`（只能下载一次），
记下页面上的 **Key ID** 和 **Issuer ID**。

### 4. 填入 GitHub Secrets

仓库 Settings → Secrets and variables → Actions → New repository secret，或者用 gh：

| Secret | 内容 |
|---|---|
| `APPLE_CERTIFICATE` | 第 2 步 `DeveloperID.b64` 的内容 |
| `APPLE_CERTIFICATE_PASSWORD` | 导出 `.p12` 时设置的密码 |
| `APPLE_SIGNING_IDENTITY` | 证书完整名称 |
| `APPLE_API_ISSUER` | Issuer ID |
| `APPLE_API_KEY` | Key ID |
| `APPLE_API_KEY_P8` | `AuthKey_XXXXXXXXXX.p8` 文件的全部内容 |

```bash
gh secret set APPLE_CERTIFICATE --repo AnixOps/EasySSH < DeveloperID.b64
gh secret set APPLE_CERTIFICATE_PASSWORD --repo AnixOps/EasySSH
gh secret set APPLE_SIGNING_IDENTITY --repo AnixOps/EasySSH
gh secret set APPLE_API_ISSUER --repo AnixOps/EasySSH
gh secret set APPLE_API_KEY --repo AnixOps/EasySSH
gh secret set APPLE_API_KEY_P8 --repo AnixOps/EasySSH < AuthKey_XXXXXXXXXX.p8
```

不带 `<` 的命令会提示你粘贴值，不会出现在命令历史里。

### 5. 发布

之后打的每个 `vX.Y.Z` 标签都会签名并公证 macOS 安装包；发布说明会自动去掉
“右键打开”的提示。公证通常需要几分钟，macOS 构建会因此变慢一些。
