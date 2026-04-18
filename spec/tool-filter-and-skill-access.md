# Tool Filter + Sub-Agent Skill Access — Implementation Plan

## Date: 2026-04-18
## Branch: orc_tool
## Status: Plan

---

## Part 1: ลด tools ที่ main agent เห็น

### หลักการ

เพิ่ม `allowed_tools` config สำหรับ main agent ใน `[agent]` section
เพื่อ filter tools ที่ส่งให้ LLM — ลด noise ให้ Gemini call tool ได้ดีขึ้น

### Tools ที่ตัดออก (ตาม ._reference/deep_agents_request/unused_tool)

ตัดทั้งกลุ่ม:

| กลุ่ม | Tools | จำนวน |
|-------|-------|:---:|
| **SOP** | sop_execute, sop_list, sop_status, sop_advance, sop_approve | 5 |
| **CLI tools** | codex_cli, claude_code, claude_code_runner, gemini_cli, opencode_cli | 5 |
| **Hardware** | hardware_board_info, hardware_memory_map, hardware_memory_read | 3 |
| **Cloud/DevOps** | cloud_ops, cloud_patterns, security_ops, git_operations, backup, data_management | 6 |
| **Integrations** | notion, jira, discord_search, google_workspace, microsoft365, linkedin, composio, pushover | 8 |

ตัดรายตัว:

| Tool | เหตุผล |
|------|--------|
| text_browser | ซ้ำกับ web_fetch |
| browser_delegate | ซ้ำกับ browser |
| escalate_to_human | ไม่มี human operator |
| poll | ไม่ใช้กับ Telegram bot |
| reaction | ไม่จำเป็น |
| model_switch | หัวหน้าตั้ง model ไว้แล้ว user ไม่ต้องสลับ |
| model_routing_config | config routing ภายใน |
| llm_task | ส่งงานไป LLM อื่น — internal |
| proxy_config | แก้ config proxy — internal |
| schedule | shell-only job, output ไม่ส่งไป Telegram — ไม่มีประโยชน์กับ 9Nerd |

**รวมตัด: 37 tools**

### Tools ที่เก็บไว้ (48 tools)

| กลุ่ม | Tools | จำนวน |
|-------|-------|:---:|
| **Core** | shell, file_read, file_write, file_edit, glob_search, content_search | 6 |
| **Web** | http_request, web_fetch, web_search_tool, browser, browser_open | 5 |
| **Cron** | cron_add, cron_list, cron_remove, cron_update, cron_run, cron_runs | 6 |
| **Memory** | memory_recall, memory_store, memory_forget, memory_purge, memory_export | 5 |
| **Media** | image_gen, image_info, pdf_read, screenshot, canvas | 5 |
| **Sessions** | sessions_history, sessions_list, sessions_send, ask_user | 4 |
| **Utility** | calculator, weather, knowledge, project_intel, report_template, vi_verify | 6 |
| **Internal** | tool_search, read_skill, workspace | 3 |
| **Orchestration** | delegate, swarm, orchestrate, execute_pipeline | 4 |

**รวมเก็บ: 48 tools** (จาก 85 → 48, ลด 44%)

### วิธี Implement

#### Option A: Config-based filter (แก้ Rust ~15 LOC)

เพิ่ม `excluded_tools` ใน `[agent]` config:

```toml
[agent]
excluded_tools = [
    # SOP (5)
    "sop_execute", "sop_list", "sop_status", "sop_advance", "sop_approve",
    # CLI tools (5)
    "codex_cli", "claude_code", "claude_code_runner", "gemini_cli", "opencode_cli",
    # Hardware (3)
    "hardware_board_info", "hardware_memory_map", "hardware_memory_read",
    # Cloud/DevOps (6)
    "cloud_ops", "cloud_patterns", "security_ops", "git_operations", "backup", "data_management",
    # Integrations (8)
    "notion", "jira", "discord_search", "google_workspace", "microsoft365",
    "linkedin", "composio", "pushover",
    # Redundant (2)
    "text_browser", "browser_delegate",
    # Unused (5)
    "escalate_to_human", "poll", "reaction",
    # Internal (5)
    "model_switch", "model_routing_config", "llm_task", "proxy_config", "schedule",
]
```

**ไฟล์ที่แก้:**

1. `src/config/schema.rs` — เพิ่ม field ใน AgentConfig:
```rust
pub struct AgentConfig {
    // ... existing ...
    /// Tools to exclude from the main agent's tool list.
    /// Tools listed here will not be sent to the LLM.
    #[serde(default)]
    pub excluded_tools: Vec<String>,
}
```

2. `src/agent/agent.rs` — ใน `from_config()` filter tools ก่อนส่งให้ builder:
```rust
// line ~555 (before Agent::builder())
let excluded: HashSet<&str> = config.agent.excluded_tools
    .iter().map(|s| s.as_str()).collect();
let tools: Vec<Box<dyn Tool>> = tools
    .into_iter()
    .filter(|t| !excluded.contains(t.name()))
    .collect();
```

3. ไม่ต้องแก้ `mod.rs` — tools ยังถูก register ทั้งหมด (sub-agents ยังใช้ได้) แต่ main agent ไม่เห็น

**ข้อดี `excluded_tools` vs `allowed_tools`:**
- `excluded_tools` = ระบุแค่ตัวที่ไม่ต้องการ — ไม่ต้อง list tools 53 ตัว
- ถ้า ZeroClaw เพิ่ม tool ใหม่ใน upstream → auto เห็น ไม่ต้องแก้ config
- Sub-agents (delegate, orchestrator) ยังเข้าถึง tools ทั้งหมดผ่าน parent_tools

---

## Part 2: Sub-Agent Skill Access (Phase 1.5)

### ปัจจุบัน

Orchestrator sub-agents **ไม่เห็น skills เลย** — system prompt มีแค่ Tools + Safety + DateTime

### เพิ่ม `allowed_skills` field

**ไฟล์ที่แก้:**

1. `src/config/schema.rs` — เพิ่ม field ใน DelegateAgentConfig:
```rust
pub struct DelegateAgentConfig {
    // ... existing ...
    /// Optional skill names to load for this agent.
    /// When empty (default), no skills are loaded (orchestrator keeps context small).
    /// When set, only skills matching these names are injected into the system prompt.
    #[serde(default)]
    pub allowed_skills: Vec<String>,
}
```

2. `src/tools/orchestrator.rs` — แก้ `build_enriched_system_prompt()`:
```rust
fn build_enriched_system_prompt(
    &self,
    agent_config: &DelegateAgentConfig,
    sub_tools: &[Box<dyn Tool>],
    workspace_dir: &Path,
) -> Option<String> {
    // Load and filter skills if allowed_skills is set
    let skills = if agent_config.allowed_skills.is_empty() {
        vec![]
    } else {
        let skills_dir = agent_config
            .skills_directory
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .map(|dir| workspace_dir.join(dir))
            .unwrap_or_else(|| crate::skills::skills_dir(workspace_dir));
        let all_skills = crate::skills::load_skills_from_directory(&skills_dir, false);
        all_skills
            .into_iter()
            .filter(|s| agent_config.allowed_skills.iter().any(|name| name == &s.name))
            .collect()
    };

    let ctx = PromptContext {
        // ... existing ...
        skills: &skills,  // filtered skills instead of &[]
        // ...
    };

    let mut builder = SystemPromptBuilder::default()
        .add_section(Box::new(ToolsSection))
        .add_section(Box::new(SafetySection))
        .add_section(Box::new(DateTimeSection));

    // Only add SkillsSection if there are skills to show
    if !skills.is_empty() {
        builder = builder.add_section(Box::new(SkillsSection));
    }

    // ... rest same as current ...
}
```

3. เพิ่ม `allowed_skills` default ใน Config initializers (schema.rs, wizard.rs)

### Config ตัวอย่าง

```toml
# Agent ที่ไม่ต้องการ skill (default — context เล็ก)
[agents.planner]
agentic = false
allowed_tools = []
allowed_skills = []          # ← default: ไม่โหลด skill

# Agent ที่ต้องการ skill เฉพาะ
[agents.cron_writer]
agentic = true
allowed_tools = ["cron_add"]
allowed_skills = ["scheduled-jobs"]   # ← โหลดแค่ skill นี้

# Agent ที่ต้องการหลาย skills
[agents.researcher]
agentic = true
allowed_tools = ["shell", "http_request"]
allowed_skills = ["google-search", "browser"]   # ← โหลด 2 skills
```

---

## สรุปทั้ง 2 Parts

| Part | ทำอะไร | ไฟล์ที่แก้ | LOC |
|------|--------|-----------|:---:|
| **1: excluded_tools** | Main agent ไม่เห็น tools ที่ไม่ใช้ | schema.rs, agent.rs | ~15 |
| **2: allowed_skills** | Sub-agent เลือกโหลด skill ได้ | schema.rs, orchestrator.rs, wizard.rs | ~30 |
| **รวม** | | | **~45** |

### ลำดับการทำ

```
1. เพิ่ม excluded_tools ใน AgentConfig (schema.rs)
2. เพิ่ม filter logic ใน agent.rs from_config()
3. เพิ่ม allowed_skills ใน DelegateAgentConfig (schema.rs)
4. แก้ orchestrator.rs build_enriched_system_prompt()
5. เพิ่ม default values ใน Config initializers
6. cargo check
7. เขียน unit tests
8. cargo test
```

### ผลลัพธ์

| | ก่อน | หลัง |
|---|---|---|
| Main agent tools | 85 | **48** (ลด 44%) |
| Tool descriptions ใน system prompt | ~8,000 bytes | **~4,500 bytes** |
| Sub-agent skills | ไม่มีเลย | **เลือกได้รายตัว** |
| Sub-agent context | ~500-700 bytes | ~500-2,000 bytes (ขึ้นกับ skill) |
