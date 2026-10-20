// Passive-scan regex rules — all in one place so they're easy to add or edit.
//
// Each rule matches request/response TEXT. To add one, copy a line and change:
//   id       unique key (also used to group findings)
//   title    shown in the Passive view
//   severity 'high' | 'medium' | 'low' | 'info'
//   pattern  a regex as a STRING (double the backslashes: \\b, \\d, \\.)
//   flags    regex flags; 'g' is always added, add 'i' for case-insensitive
//   group    (optional) capture group number to report instead of the whole match
//
// The same shape is used by custom rules added in the UI (see PassiveView), so
// anything you can write here you can also add at runtime.
//
// The Rust MCP scanner mirrors these (src-tauri/src/mcp.rs::BUILTIN_RULES). Rust's
// `regex` crate has no lookaround, so a couple of rules that rely on lookbehind/
// lookahead here (e.g. `email`) are matched broadly there and refined in code.

// File/asset/code extensions that look like a TLD but aren't, so `name@app.js`
// stops being reported as an email. None of these are real TLDs, so no genuine
// address is dropped. Shared with the Rust scanner (keep the two in sync).
export const EMAIL_TLD_DENYLIST = [
  'js', 'jsx', 'ts', 'tsx', 'mjs', 'cjs', 'css', 'scss', 'sass', 'less', 'map',
  'json', 'html', 'htm', 'xml', 'vue', 'svelte', 'png', 'jpg', 'jpeg', 'gif',
  'svg', 'webp', 'bmp', 'ico', 'woff', 'ttf', 'eot', 'otf', 'wasm', 'lock',
  'min', 'txt', 'yaml', 'yml', 'toml', 'ini', 'cfg', 'conf', 'class', 'jar',
  'war', 'dll', 'exe', 'node', 'bin', 'php', 'aspx', 'jsp', 'go', 'rb', 'java',
  'cpp', 'hpp',
];
const EMAIL_TLD_ALT = 'js|jsx|ts|tsx|mjs|cjs|css|scss|sass|less|map|json|html|htm|xml|vue|svelte|png|jpe?g|gif|svg|webp|bmp|ico|woff|ttf|eot|otf|wasm|lock|min|txt|yaml|yml|toml|ini|cfg|conf|class|jar|war|dll|exe|node|bin|php|aspx|jsp|go|rb|java|cpp|hpp';

export const BUILTIN_RULES = [
  { id: 'jwt', title: 'JSON Web Token', severity: 'medium', pattern: '\\beyJ[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{8,}\\.[A-Za-z0-9_-]{4,}', flags: 'g' },
  { id: 'aws-key', title: 'AWS access key ID', severity: 'high', pattern: '\\b(?:AKIA|ASIA)[0-9A-Z]{16}\\b', flags: 'g' },
  { id: 'google-key', title: 'Google API key', severity: 'high', pattern: '\\bAIza[0-9A-Za-z_-]{35}\\b', flags: 'g' },
  { id: 'slack-token', title: 'Slack token', severity: 'high', pattern: '\\bxox[baprs]-[0-9A-Za-z-]{10,}', flags: 'g' },
  { id: 'github-token', title: 'GitHub token', severity: 'high', pattern: '\\bgh[posru]_[0-9A-Za-z]{36}\\b', flags: 'g' },
  { id: 'stripe-key', title: 'Stripe secret key', severity: 'high', pattern: '\\b(?:sk|rk)_live_[0-9A-Za-z]{10,}\\b', flags: 'g' },
  { id: 'private-key', title: 'Private key block', severity: 'high', pattern: '-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----', flags: 'g' },
  { id: 'secret-assign', title: 'Hardcoded secret', severity: 'medium', group: 1, flags: 'gi', pattern: `\\b(?:api[_-]?key|secret|passwd|password|access[_-]?token|client[_-]?secret|authorization)\\b["']?\\s*[:=]\\s*["']([^"'\\s]{6,})["']` },

  // Credentials in transit / URLs.
  { id: 'authorization-basic', title: 'HTTP Basic credentials', severity: 'high', pattern: 'Authorization:\\s*Basic\\s+[A-Za-z0-9+/=]{8,}', flags: 'gi' },
  { id: 'bearer-token', title: 'Bearer token', severity: 'medium', pattern: 'Authorization:\\s*Bearer\\s+[A-Za-z0-9._~+/-]{10,}', flags: 'gi' },
  { id: 'creds-in-url', title: 'Credentials in URL', severity: 'high', pattern: '\\b[a-z][a-z0-9+.-]*://[^/\\s:@]+:[^/\\s:@]+@', flags: 'gi' },
  { id: 'password-field', title: 'Password in query/body', severity: 'medium', flags: 'gi', pattern: `(?:password|passwd|pwd)=[^&\\s"']{3,}` },

  // More provider keys/tokens.
  { id: 'openai-key', title: 'OpenAI API key', severity: 'high', pattern: '\\bsk-[A-Za-z0-9]{20,}', flags: 'g' },
  { id: 'google-oauth', title: 'Google OAuth access token', severity: 'high', pattern: '\\bya29\\.[A-Za-z0-9_-]{20,}', flags: 'g' },
  { id: 'slack-webhook', title: 'Slack webhook URL', severity: 'high', pattern: 'https://hooks\\.slack\\.com/services/[A-Za-z0-9/]+', flags: 'g' },
  { id: 'sendgrid-key', title: 'SendGrid API key', severity: 'high', pattern: '\\bSG\\.[A-Za-z0-9_-]{16,}\\.[A-Za-z0-9_-]{16,}', flags: 'g' },
  { id: 'twilio-sid', title: 'Twilio account SID', severity: 'medium', pattern: '\\bAC[0-9a-fA-F]{32}\\b', flags: 'g' },
  { id: 'npm-token', title: 'npm token', severity: 'high', pattern: '\\bnpm_[A-Za-z0-9]{36}\\b', flags: 'g' },

  // Bundler-exposed / cloud provider secrets.
  { id: 'vite-exposed-secret', title: 'Bundler-exposed secret (VITE_/NEXT_PUBLIC_/…)', severity: 'high', group: 1, flags: 'g', pattern: `\\b(?:VITE|NEXT_PUBLIC|REACT_APP|VUE_APP|EXPO_PUBLIC|NG_APP|PUBLIC)_[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASS(?:WORD)?|API|AUTH|PRIVATE|CRED(?:ENTIAL)?)[A-Z0-9_]*["']?\\s*[:=]\\s*["']([^"'\\s]{6,})["']` },
  { id: 'azure-storage-key', title: 'Azure Storage account key', severity: 'high', flags: 'gi', pattern: 'AccountKey=[A-Za-z0-9+/]{86}==' },
  { id: 'azure-storage-conn', title: 'Azure Storage connection string', severity: 'high', flags: 'gi', pattern: 'DefaultEndpointsProtocol=https?;AccountName=[a-z0-9]+;AccountKey=[A-Za-z0-9+/=]{80,}' },
  { id: 'azure-ad-secret', title: 'Azure AD client secret', severity: 'high', pattern: '\\b[A-Za-z0-9._~-]{2,5}Q~[A-Za-z0-9._~-]{28,36}\\b', flags: 'g' },
  { id: 'gcp-service-account', title: 'GCP service-account key file', severity: 'high', pattern: '"type"\\s*:\\s*"service_account"', flags: 'g' },
  { id: 'google-oauth-client-secret', title: 'Google OAuth client secret', severity: 'high', pattern: '\\bGOCSPX-[A-Za-z0-9_-]{28}\\b', flags: 'g' },
  { id: 'github-fine-grained-pat', title: 'GitHub fine-grained PAT', severity: 'high', pattern: '\\bgithub_pat_[0-9A-Za-z_]{82}\\b', flags: 'g' },
  { id: 'gitlab-pat', title: 'GitLab personal access token', severity: 'high', pattern: '\\bglpat-[0-9A-Za-z_-]{20}\\b', flags: 'g' },
  { id: 'shopify-token', title: 'Shopify access token', severity: 'high', pattern: '\\bshp(?:at|ss|ca|pa)_[a-fA-F0-9]{32}\\b', flags: 'g' },
  { id: 'digitalocean-token', title: 'DigitalOcean access token', severity: 'high', pattern: '\\bdop_v1_[a-f0-9]{64}\\b', flags: 'g' },
  { id: 'firebase-fcm-key', title: 'Firebase Cloud Messaging server key', severity: 'high', pattern: '\\bAAAA[A-Za-z0-9_-]{7}:[A-Za-z0-9_-]{140}\\b', flags: 'g' },
  { id: 'twilio-api-key', title: 'Twilio API key', severity: 'medium', pattern: '\\bSK[0-9a-fA-F]{32}\\b', flags: 'g' },
  { id: 'mailchimp-key', title: 'Mailchimp API key', severity: 'high', pattern: '\\b[0-9a-f]{32}-us[0-9]{1,2}\\b', flags: 'g' },

  // More provider tokens (distinctive prefixes -> low false-positive rate).
  { id: 'slack-app-token', title: 'Slack app-level token', severity: 'high', pattern: '\\bxapp-\\d-[A-Z0-9]+-\\d+-[a-f0-9]+\\b', flags: 'g' },
  { id: 'telegram-bot-token', title: 'Telegram bot token', severity: 'high', pattern: '\\b\\d{8,10}:AA[A-Za-z0-9_-]{33}\\b', flags: 'g' },
  { id: 'discord-bot-token', title: 'Discord bot token', severity: 'high', pattern: '\\b[MNO][A-Za-z0-9_-]{23}\\.[A-Za-z0-9_-]{6}\\.[A-Za-z0-9_-]{27}\\b', flags: 'g' },
  { id: 'square-access-token', title: 'Square access token', severity: 'high', pattern: '\\bsq0atp-[0-9A-Za-z_-]{22}\\b', flags: 'g' },
  { id: 'square-oauth-secret', title: 'Square OAuth secret', severity: 'high', pattern: '\\bsq0csp-[0-9A-Za-z_-]{43}\\b', flags: 'g' },
  { id: 'pypi-token', title: 'PyPI upload token', severity: 'high', pattern: '\\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,}', flags: 'g' },
  { id: 'postman-key', title: 'Postman API key', severity: 'high', pattern: '\\bPMAK-[0-9a-fA-F]{24}-[0-9a-fA-F]{34}\\b', flags: 'g' },
  { id: 'linear-key', title: 'Linear API key', severity: 'high', pattern: '\\blin_api_[0-9A-Za-z]{40}\\b', flags: 'g' },
  { id: 'databricks-token', title: 'Databricks personal access token', severity: 'high', pattern: '\\bdapi[0-9a-f]{32}\\b', flags: 'g' },
  { id: 'figma-token', title: 'Figma personal access token', severity: 'high', pattern: '\\bfigd_[0-9A-Za-z_-]{40,}', flags: 'g' },
  { id: 'dynatrace-token', title: 'Dynatrace API token', severity: 'high', pattern: '\\bdt0[a-z]0[0-9]\\.[A-Z0-9]{24}\\.[A-Z0-9]{64}\\b', flags: 'g' },
  { id: 'vault-token', title: 'HashiCorp Vault token', severity: 'high', pattern: '\\bhvs\\.[A-Za-z0-9_-]{24,}', flags: 'g' },
  { id: 'cloudinary-url', title: 'Cloudinary credentials URL', severity: 'high', pattern: '\\bcloudinary://\\d+:[A-Za-z0-9_-]+@[a-z0-9-]+', flags: 'g' },
  { id: 'age-secret-key', title: 'age secret key', severity: 'high', pattern: '\\bAGE-SECRET-KEY-1[0-9A-Z]{58}\\b', flags: 'g' },

  // Interesting data.
  { id: 'pem-cert', title: 'Certificate block', severity: 'low', pattern: '-----BEGIN CERTIFICATE-----', flags: 'g' },
  { id: 'private-ip', title: 'Private IP address', severity: 'info', pattern: '\\b(?:10\\.(?:\\d{1,3}\\.){2}\\d{1,3}|192\\.168\\.\\d{1,3}\\.\\d{1,3}|172\\.(?:1[6-9]|2\\d|3[01])\\.\\d{1,3}\\.\\d{1,3})\\b', flags: 'g' },
  // Tightened to cut false positives: the local part starts and ends with an
  // alphanumeric and never doubles a separator (kills garbage like `6..@qA.AR`),
  // the domain's first label must start with a letter (kills asset refs like
  // logo@2x.png), a lookbehind skips URL userinfo (https://key@host), and a
  // negative lookahead drops code/asset extensions that look like a TLD
  // (main@bundle.js, theme@dark.css — see EMAIL_TLD_DENYLIST).
  { id: 'email', title: 'Email address', severity: 'info', flags: 'gi', pattern: `(?<![\\w./@-])[a-z0-9]+(?:[._%+-][a-z0-9]+)*@[a-z][a-z0-9-]*(?:\\.[a-z0-9-]+)*\\.(?!(?:${EMAIL_TLD_ALT})\\b)[a-z]{2,24}\\b` },
];
