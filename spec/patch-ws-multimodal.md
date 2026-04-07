# Patch: WebSocket /ws/chat Multimodal Support + Document MIME Types

## ปัญหา 2 เรื่อง

### 1. `/ws/chat` ไม่ process `[IMAGE:]` markers

`agent.rs:turn_streamed()` สร้าง messages แล้วส่งตรงไป `stream_chat` โดยไม่เรียก `prepare_messages_for_provider()`

```rust
// agent.rs:957 — ปัจจุบัน
let messages = self.tool_dispatcher.to_provider_messages(&self.history);
// ... ส่ง messages ตรงไป stream_chat โดยไม่ผ่าน multimodal pipeline
```

ในขณะที่ `loop_.rs:2469` (full agent loop) ทำถูก:
```rust
let prepared_messages =
    multimodal::prepare_messages_for_provider(history, multimodal_config).await?;
```

### 2. `ALLOWED_IMAGE_MIME_TYPES` ไม่รองรับ PDF

```rust
// multimodal.rs:8-14
const ALLOWED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp",
];
// ไม่มี "application/pdf" — ทั้งที่ Gemini และ Anthropic รองรับ
```

---

## แผนแก้ไข

### Fix 1: เพิ่ม multimodal pipeline ใน `turn_streamed()`

#### Step 1.1: เพิ่ม `multimodal_config` ใน Agent struct

```rust
// agent.rs:40 — เพิ่ม field
pub struct Agent {
    // ... existing fields ...
    multimodal_config: crate::config::MultimodalConfig,  // ← เพิ่ม
}
```

#### Step 1.2: ตั้งค่าใน `from_config()`

```rust
// agent.rs:from_config() — เพิ่มตอนสร้าง Agent
Agent {
    // ... existing fields ...
    multimodal_config: config.multimodal.clone(),  // ← เพิ่ม
}
```

#### Step 1.3: เรียก `prepare_messages_for_provider` ใน `turn_streamed()`

```rust
// agent.rs:957 — แก้จาก:
let messages = self.tool_dispatcher.to_provider_messages(&self.history);

// เป็น:
let messages = self.tool_dispatcher.to_provider_messages(&self.history);
let prepared = crate::multimodal::prepare_messages_for_provider(
    &messages, &self.multimodal_config
).await?;
let messages = prepared.messages;
```

เพิ่ม import ด้านบนของไฟล์:
```rust
use crate::multimodal;
```

### Fix 2: เพิ่ม Document MIME Types

#### Step 2.1: เปลี่ยนชื่อ constant + เพิ่ม mime types

```rust
// multimodal.rs:8 — แก้จาก:
const ALLOWED_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp",
];

// เป็น:
const ALLOWED_MIME_TYPES: &[&str] = &[
    // Images
    "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp",
    // Documents (supported by Gemini, Anthropic)
    "application/pdf",
];
```

#### Step 2.2: อัพเดต references ทั้งหมดที่ใช้ `ALLOWED_IMAGE_MIME_TYPES`

ค้นหาและ replace ทุกที่ที่ reference constant นี้:
- `validate_mime()` function
- Test cases

---

## ไฟล์ที่ต้องแก้

| ไฟล์ | การแก้ | ความเสี่ยง |
|------|--------|-----------|
| `src/agent/agent.rs` | เพิ่ม `multimodal_config` field + เรียก `prepare_messages_for_provider` ใน `turn_streamed()` | กลาง — ต้องเพิ่ม field + แก้ constructor |
| `src/multimodal.rs` | เพิ่ม `application/pdf` ใน allowed mime types | ต่ำ — เพิ่ม string ใน array |
| `src/agent/agent.rs` (tests) | อัพเดต test ที่สร้าง Agent ให้มี `multimodal_config` | ต่ำ |

## ขั้นตอน Build + Test

```bash
# 1. แก้ไขไฟล์
# 2. Build
cargo build --release

# 3. Test
cargo test --lib agent::agent -- --test-threads=1

# 4. Copy binary
cp target/release/zeroclaw ../zeroclaw-container/zeroclaw

# 5. Build Docker image + test
docker build --platform linux/amd64 -f ../zeroclaw-container/Dockerfile -t zeroclaw-local:latest ../
```

## ทดสอบ

1. `/ws/chat` + `[IMAGE:/path/photo.jpg]` → ต้องอ่านรูปได้
2. `/ws/chat` + `[IMAGE:/path/file.pdf]` → ต้องอ่าน PDF ได้
3. `/webhook` → ต้องยังทำงานได้เหมือนเดิม
4. ข้อความธรรมดา (ไม่มีไฟล์) → ต้องทำงานปกติ
5. Session multi-turn → ต้องยังทำงานได้

## ข้อควรระวัง

- `turn_streamed()` เป็น async function → `prepare_messages_for_provider` เป็น async ได้
- `messages` ใช้ reference `&self.history` → ต้องระวังเรื่อง borrow checker
- ถ้า multimodal pipeline error (ไฟล์ไม่เจอ, ขนาดเกิน) → ต้อง handle gracefully ไม่ให้ crash
- PDF support อาจไม่ work กับทุก provider — ต้องเช็ค provider capabilities
