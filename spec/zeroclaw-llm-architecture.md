# ZeroClaw LLM Request Architecture

รายละเอียดว่า ZeroClaw ส่งอะไรให้ LLM ในแต่ละ turn

---

## 1. System Prompt (ส่งทุก turn ใน field `system` ของ request)

สร้างครั้งเดียวตอนเริ่ม session แต่ **ส่งซ้ำทุก turn** เพราะ LLM API เป็น stateless — ทุก request ต้องแนบ system prompt มาด้วย
Anthropic API จะ **cache** system prompt ที่ >3KB ไว้อัตโนมัติ (ไม่ประมวลผลซ้ำ, ประหยัด token)

ส่งแยกจาก `messages[]` ใน field `system` ของ request payload:

```json
{
  "system": [{ "type": "text", "text": "[sections ด้านล่าง]", "cache_control": {"type": "ephemeral"} }],
  "messages": [...],
  "tools": [...]
}
```

สร้างแบบ modular จาก `src/agent/prompt.rs` โดย `SystemPromptBuilder::with_defaults()` ประกอบด้วย sections ตามลำดับ:

1. **DateTimeSection** — วันเวลาปัจจุบัน + timezone
2. **IdentitySection** — Project context, AGENTS.md, AIEOS config, personality profiles
3. **ToolHonestySection** — ห้าม LLM สร้างผลลัพธ์ tool ขึ้นมาเอง
4. **ToolsSection** — รายชื่อ tools ทั้งหมด + parameter JSON schema
5. **SafetySection** — คำเตือน data exfiltration, autonomy level, security policy
6. **SkillsSection** — Skills ที่ลงทะเบียนไว้ + callable tools (ดูรายละเอียดใน section 1.1)
7. **WorkspaceSection** — working directory path
8. **RuntimeSection** — Host, OS, Model name
9. **ChannelMediaSection** — อธิบาย markers [Voice], [IMAGE:], [Document:]

หมายเหตุ: system prompt >3KB จะถูก cache (Anthropic) เพื่อประหยัด token

### 1.1 SkillsSection รายละเอียด

จาก `src/skills/mod.rs:761-864` + `src/agent/prompt.rs:217-229`

#### Skill Data Structure (`src/skills/mod.rs:34-61`)

```rust
pub struct Skill {
    pub name: String,           // ชื่อ skill
    pub description: String,    // คำอธิบายสั้น
    pub version: String,
    pub author: Option<String>,
    pub tags: Vec<String>,
    pub tools: Vec<SkillTool>,  // tools ที่ skill มี
    pub prompts: Vec<String>,   // instructions สำหรับ LLM
    pub location: Option<PathBuf>,
}

pub struct SkillTool {
    pub name: String,           // ชื่อ tool (e.g., "run_lint")
    pub description: String,
    pub kind: String,           // "shell", "script", or "http"
    pub command: String,        // command/URL template
    pub args: HashMap<String, String>,
}
```

#### 2 โหมดการส่ง (config: `skills.prompt_injection_mode`)

**Full Mode (default)** — ส่งทั้งหมดไปใน system prompt:

```xml
## Available Skills

<available_skills>
  <skill>
    <name>deploy</name>
    <description>Release safely</description>
    <location>/path/to/skills/deploy/SKILL.md</location>
    <instructions>
      <instruction>Run smoke tests before deploy.</instruction>
      <instruction>...</instruction>
    </instructions>
    <callable_tools>
      <tool>
        <name>deploy.release_checklist</name>
        <description>Validate release readiness</description>
      </tool>
    </callable_tools>
  </skill>
</available_skills>
```

**Compact Mode** — ส่งแค่ header, ให้ LLM เรียก `read_skill(name)` เอง:

```xml
## Available Skills

Skill summaries are preloaded below to keep context compact.
Call `read_skill(name)` when you need the full skill file.

<available_skills>
  <skill>
    <name>deploy</name>
    <description>Release safely</description>
    <location>skills/deploy/SKILL.md</location>
    <!-- ไม่มี <instructions> — ประหยัด token -->
    <callable_tools>
      <tool>
        <name>deploy.release_checklist</name>
        <description>Validate release readiness</description>
      </tool>
    </callable_tools>
  </skill>
</available_skills>
```

#### สรุปความแตกต่าง

| | Full Mode | Compact Mode |
|---|-----------|-------------|
| name + description | ✅ ส่ง | ✅ ส่ง |
| location | ✅ ส่ง | ✅ ส่ง |
| instructions/prompts | ✅ ส่งทั้งหมด | ❌ ไม่ส่ง |
| callable tools | ✅ ส่ง | ✅ ส่ง |
| non-callable tools | ✅ ส่ง (doc only) | ✅ ส่ง (doc only) |
| on-demand loading | ไม่จำเป็น | ใช้ `read_skill(name)` |

#### Tool Name Prefix

Tools ของ skill จะถูก prefix ด้วยชื่อ skill: `{skillname}.{toolname}`
เช่น skill "deploy" มี tool "release_checklist" → LLM เรียกได้ด้วยชื่อ `deploy.release_checklist`

#### XML Escaping

ทุก text value ถูก escape: `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, `"` → `&quot;`, `'` → `&apos;`

#### Key Files

| ไฟล์ | หน้าที่ |
|------|---------|
| `src/agent/prompt.rs:217-229` | SkillsSection implementation |
| `src/skills/mod.rs:761-864` | Rendering logic (Full/Compact) |
| `src/skills/mod.rs:34-61` | Skill/SkillTool struct definitions |
| `src/skills/mod.rs:706-730` | XML escaping |
| `src/config/schema.rs:1656-1662` | SkillsPromptInjectionMode enum |
| `src/tools/read_skill.rs` | read_skill tool (Compact mode) |
| `src/tools/skill_tool.rs` | Skill shell/http tool execution |

#### SKILL.md Format

ZeroClaw รองรับ YAML frontmatter (optional):

```markdown
---
name: browser
description: Open websites, take screenshots, read content
version: 1.0.0
author: 9nerd
tags:
  - browser
  - automation
---
(body — ส่งเป็น prompts[0] ให้ LLM)
```

- มี frontmatter → parse name/description/version/author/tags จาก frontmatter, body หลัง `---` เป็น `prompts[0]`
- ไม่มี frontmatter → name = ชื่อโฟลเดอร์, description infer จากเนื้อหา, ทั้งไฟล์เป็น `prompts[0]`
- tools ประกาศใน SKILL.md ไม่ได้ — ต้องใช้ SKILL.toml

#### 9Nerd Decision: ใช้ Full Mode

เหตุผล:
1. **3 skills / ~12.8 KB / ~3,500-4,000 tokens** — ยังไม่กิน context มาก
2. **`file-org` + `browser` ใช้เกือบทุก turn** — Compact ต้อง `read_skill` แทบทุกครั้งอยู่ดี เสีย latency เปล่า
3. **ลดโอกาสผิดพลาด** — browser skill มี rules สำคัญ (ห้าม guess selector, ต้อง snapshot ก่อน click) ถ้า LLM ไม่เห็นอาจทำผิด
4. **Compact คุ้มเมื่อมี 10+ skills / 50KB+** — ยังไม่ถึงจุดนั้น

| Skill | ขนาด | ความถี่ใช้ |
|-------|-------|-----------|
| `browser` | 5.7 KB / 139 lines | บ่อย |
| `file-org` | 2.6 KB / 86 lines | เกือบทุกครั้ง |
| `scheduled-jobs` | 4.5 KB / 113 lines | นานๆ ที |

---

## 2. User Message Enrichment (ทุก turn)

คำถาม/ข้อความจาก user อยู่ใน `messages[]` ตัว**สุดท้าย** ที่มี `role: "user"` — แต่ไม่ได้ส่ง raw ตรงๆ จะถูก enrich ก่อนเสมอ:

```json
{
  "system": [...],
  "messages": [
    { "role": "user", "content": "ข้อความเก่า turn 1 (enriched)" },
    { "role": "assistant", "content": "คำตอบ turn 1" },
    { "role": "user", "content": "tool_result จาก turn 1" },
    { "role": "assistant", "content": "คำตอบ turn 1 (ต่อ)" },
    ...
    { "role": "user", "content": "← ข้อความใหม่ของ user อยู่ตรงนี้ (enriched)" }
  ],
  "tools": [...]
}
```

จาก `src/agent/agent.rs` (lines 1019-1046) ก่อนส่ง user message จะถูก enrich:

1. **Memory context** — ดึงจาก semantic search พร้อม time decay (Core memories ไม่มี decay)
   - Format: `[Memory context]\n- {key}: {content}\n[/Memory context]`
   - กรอง autosave keys และ tool_result blocks ออก
2. **Timestamp** — `[YYYY-MM-DD HH:MM:SS TZ] {user_message}`
3. **Hardware context** (ถ้าเปิด peripherals) — GPIO pin aliases, RAG documentation chunks
4. **Multimodal** — `[IMAGE:<path>]` markers แปลงเป็น base64 data URI

---

## 3. Conversation History (ทุก turn)

ส่งประวัติทั้งหมด (user + assistant + tool results) จาก `src/agent/loop_.rs`:

- ถ้า context tokens เกิน budget → **trim**:
  - Fast trim: ลบ tool results ก่อน
  - Deep pruning: collapse message pairs เก่า, เก็บ messages ล่าสุดไว้เสมอ
- Message count ถูก log ใน observability events

---

## 4. Tool Definitions (ทุก turn)

ส่งเป็น `tools[]` ใน request payload:

- แต่ละ tool มี: `name`, `description`, `input_schema` (JSON Schema)
- Tools ถูก **filter ตาม turn**:
  - Tool filter groups (always/dynamic mode)
  - Excluded tools list
  - Activated MCP tools
  - User message keywords (สำหรับ dynamic filtering)
- **Native format** (Anthropic/OpenAI): ส่งเป็น `ToolSpec` แยกจาก system prompt
- **XML format** (models อื่น): ใส่ tool descriptions ไว้ใน system prompt

---

## 5. Tool Result Flow

จาก `src/agent/dispatcher.rs` + `src/agent/loop_.rs`:

```
LLM response มี tool_call (id, name, arguments)
       ↓
Execute tool locally
       ↓
Scrub credentials (token/api_key/password/secret/bearer/credential)
  → เก็บ 4 ตัวแรก + *[REDACTED]
       ↓
สร้าง ToolResultMessage { tool_call_id, content }
       ↓
Append เข้า history → ส่งใน turn ถัดไป
```

### Format ตาม Provider:
- **Anthropic**: `role: "user"` + `type: "tool_result"` + `tool_use_id`
- **OpenAI**: `role: "tool"` + `tool_call_id`
- **XML** (models อื่น): `<tool_result name="..." status="ok">...</tool_result>`

---

## 6. Provider-specific Request (Anthropic ตัวอย่าง)

```json
{
  "model": "claude-3-5-sonnet-...",
  "max_tokens": 4096,
  "system": [{ "type": "text", "text": "[sections...]", "cache_control": {"type": "ephemeral"} }],
  "messages": [
    { "role": "user", "content": [{"type": "image", "source": {...}}, {"type": "text", "text": "[timestamp] msg"}] },
    { "role": "assistant", "content": [{"type": "text", "text": "..."}, {"type": "tool_use", "id": "tc_1", ...}] },
    { "role": "user", "content": [{"type": "tool_result", "tool_use_id": "tc_1", "content": "..."}] }
  ],
  "tools": [{ "name": "shell", "description": "...", "input_schema": {...}, "cache_control": {"type": "ephemeral"} }],
  "temperature": 0.7
}
```

---

## 7. Key Files

| ไฟล์ | หน้าที่ |
|------|---------|
| `src/agent/prompt.rs` | สร้าง system prompt (modular sections) |
| `src/agent/loop_.rs` | Main tool loop, เตรียม messages, เรียก LLM, context trimming |
| `src/agent/agent.rs` | Orchestration, `turn()` + `turn_streamed()` |
| `src/agent/dispatcher.rs` | Parse tool calls, format results, message conversion |
| `src/providers/anthropic.rs` | Anthropic API — แปลง tools เป็น NativeToolSpec, system cache |
| `src/providers/openai.rs` | OpenAI API — แปลง tools เป็น function format |
| `src/providers/traits.rs` | Provider trait, ChatRequest, ChatMessage, ToolCall types |
| `src/multimodal.rs` | แปลง image markers → base64, validate MIME types |
