# WeCom (Enterprise WeChat)

## Overview

The WeCom channel receives messages via Webhook callbacks and sends messages through the REST API. A publicly accessible callback endpoint is required, suitable for servers with public IPs or those exposed through reverse proxies.

!!! warning "Public Endpoint Required"
    WeCom requires a publicly accessible callback URL for message delivery. Ensure your service port is reachable by WeCom servers before deployment.

!!! tip "Security Verification"
    Callback messages are protected by AES encryption and SHA1 signature verification, ensuring message authenticity and integrity.

## Configuration Example

```yaml
channels:
  wecom:
    corp_id: "ww1234567890abcdef"
    agent_id: "1000002"
    secret: "your-app-secret-here"
    token: "your-callback-token"
    encoding_aes_key: "43-char-base64-encoding-aes-key-from-wecom"
    webhook_path: "/wecom"
    host: "0.0.0.0"
    port: 8084
```

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `corp_id` | Yes | — | Enterprise Corp ID |
| `agent_id` | Yes | — | Self-built app AgentId |
| `secret` | Yes | — | App Secret |
| `token` | Yes | — | Callback Token (for signature verification) |
| `encoding_aes_key` | Yes | — | Callback EncodingAESKey (43-char Base64) |
| `webhook_path` | No | `/wecom` | Callback URL path |
| `host` | No | `0.0.0.0` | Listen address |
| `port` | No | `8084` | Listen port |

## Credential Setup

### Step 1: Obtain Corp ID

1. Log in to the [WeCom Admin Console](https://work.weixin.qq.com/wework_admin/frame)
2. Navigate to **My Enterprise** → **Enterprise Info**
3. Locate and copy the **Corp ID** field

### Step 2: Create a Self-Built App

1. Go to **App Management** → **Self-Built**
2. Click **Create App**
3. Fill in the app name, logo, and visibility scope
4. After creation, record the **AgentId** and **Secret**

!!! tip "Secret Shown Only Once"
    The app Secret is displayed only once upon creation. Save it immediately. If lost, you must regenerate it.

### Step 3: Configure Callback

1. In the app details page, find **Receive Messages** → **Set API Receive**
2. Enter the callback URL: `https://your-domain.com/wecom`
3. Generate or customize the Token and EncodingAESKey
4. Click Save (WeCom will send a verification request at this point)

## Callback/Webhook Setup

### URL Verification Flow

WeCom sends a GET request to verify the callback URL when saving configuration:

```text
GET /wecom?msg_signature=xxx&timestamp=xxx&nonce=xxx&echostr=xxx
```

Verification steps:

1. Extract `msg_signature`, `timestamp`, `nonce`, and `echostr` parameters
2. Compute SHA1 signature using `token`, `timestamp`, `nonce`, and decrypted `echostr`
3. Compare the computed signature against `msg_signature`
4. If valid, return the decrypted `echostr` plaintext

```text
SHA1(sort(token, timestamp, nonce, echostr_decrypt)) == msg_signature
```

### Message Reception Flow

Regular messages arrive via POST:

```text
POST /wecom?msg_signature=xxx&timestamp=xxx&nonce=xxx

<xml>
  <ToUserName><![CDATA[corp_id]]></ToUserName>
  <Encrypt><![CDATA[encrypted_content]]></Encrypt>
  <AgentID>1000002</AgentID>
</xml>
```

Decryption flow:

1. Verify `msg_signature` (SHA1(sort(token, timestamp, nonce, encrypt)))
2. Decrypt using `encoding_aes_key` with AES
3. Parse XML to extract message content

### Access Token

Sending messages requires an `access_token`, obtained via Corp ID + Secret:

```text
GET https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid=ID&corpsecret=SECRET
```

!!! warning "Token Expiry"
    `access_token` is valid for 7200 seconds (2 hours) and is auto-refreshed by the system. Avoid frequent requests to prevent rate limiting.

## Capability Matrix

| Capability | Supported | Notes |
|-----------|-----------|-------|
| Send text | Yes | — |
| Send images | No | Not implemented |
| Send voice | No | Not implemented |
| Send files | No | Not implemented |
| Edit messages | No | WeCom does not support |
| Reactions | No | WeCom does not support |
| Group chat | No | Not implemented |
| Realtime messages | Yes | Webhook push |

## Technical Details

### Message Encryption/Decryption

Handled by the `wecom_crypto.py` module:

- Algorithm: AES-256-CBC
- Key: Base64Decode(EncodingAESKey + "="), first 32 bytes
- IV: First 16 bytes of the key
- Padding: PKCS#7

### Signature Verification

```text
signature = SHA1(sort([token, timestamp, nonce, encrypt_msg]))
```

All callback requests must pass signature verification to prevent forged requests.

## FAQ

!!! question "Q: Callback URL verification fails?"
    1. Confirm the service is running and the port is publicly accessible
    2. Verify Token and EncodingAESKey match the admin console values
    3. Confirm the callback path is correct (default `/wecom`)
    4. Check logs for signature computation results and compare with request parameters

!!! question "Q: Not receiving message pushes?"
    1. Confirm the app visibility scope includes the target users
    2. Check that the callback URL passed verification (green status in admin console)
    3. Verify firewalls are not blocking WeCom server IPs

!!! question "Q: access_token retrieval fails?"
    1. Verify Corp ID and Secret are correct
    2. Confirm the app has not been disabled
    3. Check IP allowlist settings (if configured)

!!! question "Q: How to verify messages are from WeCom?"
    Via `msg_signature` verification. The signature is based on SHA1 of Token + Timestamp + Nonce + EncryptedContent, which cannot be forged.
