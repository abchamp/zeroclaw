# ZeroClaw: Tool vs Skill

## Tool = ฟังก์ชันเดี่ยวที่ LLM เรียกใช้ได้

- มี `name`, `description`, `input_schema`, `execute()`
- LLM เรียกผ่าน `tool_use` โดยตรง
- ประกาศใน Rust code (`src/tools/`) หรือใน SKILL.toml `[[tools]]`
- ตัวอย่าง: `shell`, `file_read`, `memory_store`

### Tool Trait (`src/tools/traits.rs`)

```rust
trait Tool {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;  // JSON Schema
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult>;
}
```

### ประเภท Tool

| ประเภท | ตัวอย่าง | ประกาศที่ไหน |
|--------|----------|-------------|
| Built-in | `shell`, `file_read`, `memory_store` | Rust code `src/tools/` |
| Skill tool | `deploy.release_checklist` | SKILL.toml `[[tools]]` |
| MCP tool | `filesystem__read_file` | MCP server (JSON-RPC) |
| WASM plugin | dynamic | WASM module (feature-gated) |

### Tool Registration

- Built-in → `default_tools()` / `all_tools_with_runtime()` ใน `src/tools/mod.rs`
- Skill tools → `register_skill_tools()` แปลง SKILL.toml → `SkillShellTool` / `SkillHttpTool`
- MCP tools → `McpToolWrapper` wrap จาก MCP server
- Tools ถูก filter ตาม turn (always/dynamic mode, excluded list, keywords)

---

## Skill = แพ็คเกจ/bundle ที่รวม instructions + tools (optional)

- อยู่ใน `~/.zeroclaw/workspace/skills/{skill_name}/`
- **SKILL.md** → prompts/instructions ใส่ใน system prompt สอน LLM ว่าต้องทำยังไง
- **SKILL.toml `[[tools]]`** → ประกาศ tools ที่ LLM เรียกได้ (shell/http) ชื่อเป็น `{skill}.{tool}`
- Skill ไม่มี tools ก็ได้ — เป็นแค่ instructions ล้วนๆ ให้ LLM ใช้ built-in tools เอง

### โครงสร้าง

```
Skill = กล่อง
  ├── prompts/instructions  ← สอน LLM (ใส่ใน system prompt)
  └── tools[] (optional)    ← ฟังก์ชันที่ LLM เรียกได้จริง

Tool = ฟังก์ชันเดี่ยว ← อาจอยู่ใน Skill หรือเป็น built-in ก็ได้
```

### Skill Directory Structure

```
~/.zeroclaw/workspace/skills/
  └── my-skill/
      ├── SKILL.md      ← instructions/prompts (YAML frontmatter optional)
      └── SKILL.toml    ← tools + metadata
```

### SKILL.toml Format

```toml
[skill]
name = "my-skill"
description = "What it does"
version = "0.1.0"
author = "user"
tags = ["tag1"]

[[tools]]
name = "lint"
description = "Run linter"
kind = "shell"           # หรือ "http"
command = "eslint {{file}}"

[tools.args]
file = "File path to lint"
```

### SKILL.md Format

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

ห้าม guess selector, ต้อง snapshot ก่อน click ...
```

- มี frontmatter → parse name/description/version/author/tags, body หลัง `---` เป็น `prompts[0]`
- ไม่มี frontmatter → name = ชื่อโฟลเดอร์, ทั้งไฟล์เป็น `prompts[0]`
- tools ประกาศใน SKILL.md ไม่ได้ — ต้องใช้ SKILL.toml

---

## Skill Discovery & Loading

### Flow ตอน startup

1. Gateway เริ่ม → `load_skills_with_config()` (`src/skills/mod.rs:131`)
2. สแกน `{workspace}/skills/` ทุก subdirectory
3. **Security audit** — block symlinks, block scripts (ยกเว้น `allow_scripts = true`), file size limit 512KB
4. ลอง parse `SKILL.toml` ก่อน → fallback `SKILL.md`
5. `register_skill_tools()` แปลง tools → `SkillShellTool` / `SkillHttpTool`
6. Tool ชื่อ `{skill_name}.{tool_name}` ใส่เข้า tool registry
7. LLM เห็นใน system prompt

### ไม่มี hot reload

เพิ่ม/แก้ skill ต้อง **restart** ZeroClaw ถึงจะเห็น

### Open-Skills Repository (optional)

```toml
[skills]
open_skills_enabled = true
open_skills_dir = "$HOME/open-skills"
```

- Auto-sync ทุก 7 วันผ่าน git pull
- Source: https://github.com/besoeasy/open-skills

---

## Prompt Injection Mode (วิธีส่ง skill ให้ LLM)

### Full Mode (default, 9Nerd ใช้อันนี้)

ส่ง instructions + tools ทั้งหมดใน system prompt:

```xml
<skill>
  <name>browser</name>
  <description>Open websites...</description>
  <instructions>
    <instruction>ห้าม guess selector...</instruction>
  </instructions>
  <callable_tools>
    <tool><name>browser.screenshot</name>...</tool>
  </callable_tools>
</skill>
```

### Compact Mode

ส่งแค่ name + description → LLM เรียก `read_skill(name)` เอาเอง

### เมื่อไหร่ควรใช้ Compact

| | Full Mode | Compact Mode |
|---|-----------|-------------|
| เหมาะเมื่อ | skills น้อย (<10), ใช้บ่อย | skills เยอะ (10+), 50KB+ |
| ข้อดี | LLM เห็นทุกอย่างทันที, ลดผิดพลาด | ประหยัด context |
| ข้อเสีย | กิน tokens | เสีย latency (ต้อง read_skill ก่อน) |

---

## 9Nerd Skills ปัจจุบัน

| Skill | ขนาด | มี tools? | ความถี่ใช้ |
|-------|-------|-----------|-----------|
| `browser` | 5.7 KB / 139 lines | มี | บ่อย |
| `file-org` | 2.6 KB / 86 lines | ไม่มี (instructions only) | เกือบทุกครั้ง |
| `scheduled-jobs` | 4.5 KB / 113 lines | มี | นานๆ ที |

รวม ~12.8 KB / ~3,500-4,000 tokens — ใช้ Full mode เพราะยังไม่ถึงจุดที่ Compact คุ้ม

---

## สรุป: ไม่มี Skill ก็ทำงานได้

Built-in tools 81+ ตัวไม่ได้อยู่ใน skill — เป็น Rust code ตรงๆ ทั้งหมด

Skill เป็น**ของเสริม**สำหรับ:
- เพิ่ม instructions สอน LLM โดยไม่ต้องแก้ Rust code
- เพิ่ม custom tools (shell/http) โดยไม่ต้อง compile ใหม่

---

## Key Files

| ไฟล์ | หน้าที่ |
|------|---------|
| `src/tools/mod.rs` | Tool registry, `all_tools_with_runtime()`, `register_skill_tools()` |
| `src/tools/traits.rs` | Tool trait definition |
| `src/tools/skill_tool.rs` | SkillShellTool executor (timeout 60s, output 1MB) |
| `src/tools/skill_http.rs` | SkillHttpTool executor |
| `src/skills/mod.rs` | Skill discovery, SKILL.toml/md parsing, prompt rendering |
| `src/skills/audit.rs` | Security auditing (symlinks, scripts, size) |
| `src/agent/prompt.rs:217-229` | SkillsSection ใน system prompt |
| `src/config/schema.rs` | Config: `[skills]`, `prompt_injection_mode` |
