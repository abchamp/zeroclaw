# ZeroClaw Agent Orchestration — Research Summary

## Date: 2026-04-17

## Overview

ZeroClaw มีระบบ multi-agent orchestration ครบแล้ว ไม่ต้องสร้างใหม่ ประกอบด้วย 4 ระบบหลัก:

1. **delegate** — สั่งงาน sub-agent (sync / background / parallel)
2. **swarm** — ส่งงานให้กลุ่ม agents (sequential / parallel / router)
3. **pipeline** — chain tools ต่อกัน ส่ง result ต่อ
4. **SOP engine** — workflow หลายขั้นตอน trigger-based

---

## 1. Delegate Tool

### วิธีใช้

LLM เรียก `delegate` tool เหมือน tool อื่นๆ:

```json
{
  "action": "delegate",
  "agent": "researcher",
  "prompt": "หาข้อมูล PM2.5 API สำหรับประเทศไทย"
}
```

### 3 โหมด

| โหมด | วิธีใช้ | ผลลัพธ์ |
|------|--------|---------|
| **Sync** (default) | `delegate(agent, prompt)` | รอจนเสร็จ ได้ผลทันที |
| **Background** | `delegate(agent, prompt, background=true)` | ได้ task_id กลับมาทันที ตรวจผลทีหลัง |
| **Parallel** | `delegate(parallel=["a","b","c"], prompt)` | รันหลาย agent พร้อมกัน รวมผล |

### Agentic Mode

Sub-agent สามารถเรียก tool ได้เอง (multi-turn loop):

```toml
[agents.researcher]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
agentic = true
allowed_tools = ["shell", "http_request", "web_fetch", "web_search_tool"]
max_iterations = 10
system_prompt = "You are a research specialist. Find data sources and verify they work."
```

**ข้อจำกัด:**
- `allowed_tools` ต้องไม่ว่าง (ถ้า agentic=true)
- `delegate` tool ถูกตัดออกจาก sub-agent อัตโนมัติ (กัน infinite loop)
- max_depth default = 3 (กัน recursion)
- agentic timeout default = 300s

### Background Task Management

```json
// สั่งงาน
{"action": "delegate", "agent": "data_processor", "prompt": "...", "background": true}
→ {"task_id": "550e8400-..."}

// ตรวจผล
{"action": "check_result", "task_id": "550e8400-..."}
→ {"status": "completed", "output": "..."}

// ดูทั้งหมด
{"action": "list_results"}

// ยกเลิก
{"action": "cancel_task", "task_id": "550e8400-..."}
```

---

## 2. Swarm Tool

### 3 Strategies

| Strategy | วิธีทำงาน | Use Case |
|----------|----------|----------|
| **Sequential** | agent1 → ผล → agent2 → ผล → agent3 | Research → Analyze → Write |
| **Parallel** | agent1 + agent2 + agent3 พร้อมกัน | หลายมุมมองเดียวกัน |
| **Router** | LLM เลือก agent ที่เหมาะที่สุด | task ที่ไม่รู้ล่วงหน้าว่าใครควรทำ |

### Config

```toml
[[swarms.research_pipeline]]
agents = ["researcher", "writer"]
strategy = "sequential"
description = "Research then write"
timeout_secs = 600

[[swarms.multi_view]]
agents = ["researcher", "critic"]
strategy = "parallel"
timeout_secs = 300

[[swarms.smart_router]]
agents = ["researcher", "coder", "writer"]
strategy = "router"
router_prompt = "Pick the best specialist for this task."
timeout_secs = 300
```

### วิธีเรียก

```json
{"swarm": "research_pipeline", "prompt": "วิเคราะห์ตลาด food delivery ไทย"}
```

---

## 3. Pipeline Tool

Chain tools ต่อกัน ส่ง result ผ่าน `{{step[N].result}}`:

```json
{
  "steps": [
    {"tool": "shell", "args": {"command": "bash search.sh 'PM2.5 API Thailand'"}},
    {"tool": "http_request", "args": {"url": "{{step[0].result}}"}},
    {"tool": "cron_add", "args": {"prompt": "Use {{step[1].result}} ..."}}
  ],
  "parallel": false
}
```

Config:
```toml
[pipeline]
enabled = true
max_steps = 20
allowed_tools = ["shell", "http_request", "web_fetch", "cron_add"]
```

---

## 4. SOP Engine

Workflow แบบ trigger → steps:

```
workspace/sops/pm25-alert/
├── SOP.toml    (triggers, metadata)
└── SOP.md      (steps)
```

### Execution Modes

| Mode | Behavior |
|------|----------|
| auto | รันทุก step ไม่ต้อง approve |
| supervised | approve ก่อนเริ่ม แล้ว auto |
| step_by_step | approve ทุก step |
| deterministic | ไม่ใช้ LLM, pipe output ต่อกัน |

---

## แผน Agent สำหรับ 9Nerd Craw

### Planner Agent (Main)

Agent หลักที่ user คุยด้วย — รับคำสั่ง วางแผน แล้วสั่งลูกมือ

```toml
# ใช้ default provider (ไม่ต้อง config เพิ่ม)
# Main agent = ZeroClaw default agent
```

### ลูกมือ Agents

| Agent | หน้าที่ | Agentic? | Tools |
|-------|---------|----------|-------|
| **researcher** | search + verify data sources | ✅ | shell, http_request, web_fetch, web_search_tool |
| **writer** | สร้าง content, สรุป, แปลภาษา | ❌ | - |
| **monitor** | ตั้ง cron + ตรวจสอบ data | ✅ | shell, http_request, cron_add, cron_list |
| **analyst** | วิเคราะห์ข้อมูล, เปรียบเทียบ | ❌ | - |

### Config ตัวอย่าง

```toml
[delegate]
timeout_secs = 120
agentic_timeout_secs = 300

[agents.researcher]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
system_prompt = "You are a research specialist. Find reliable data sources, test APIs, verify data. Report findings concisely."
temperature = 0.3
agentic = true
allowed_tools = ["shell", "http_request", "web_fetch", "web_search_tool"]
max_iterations = 10

[agents.writer]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
system_prompt = "You are a content specialist. Write clear, concise content in the user's language."
temperature = 0.5
agentic = false

[agents.monitor]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
system_prompt = "You are a monitoring specialist. Set up data monitoring jobs with appropriate schedules and conditions."
temperature = 0.2
agentic = true
allowed_tools = ["shell", "http_request", "cron_add", "cron_list", "cron_remove"]
max_iterations = 10

[agents.analyst]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
system_prompt = "You are a data analyst. Compare, analyze, and provide actionable insights."
temperature = 0.3
agentic = false
```

### Flow ตัวอย่าง: "แจ้งเตือนค่าฝุ่นศรีราชา สีส้ม"

```
User: "แจ้งเตือนค่าฝุ่นศรีราชา สีส้ม"
  │
  ├── Main Agent (planner)
  │     └── delegate(agent="researcher", prompt="Find PM2.5 API for Sriracha, test it")
  │           ├── researcher: google-search → find API
  │           ├── researcher: http_request → test API
  │           └── return: "air4thai API works, station 32t, สีส้ม = color_id 3"
  │
  ├── Main Agent ได้ข้อมูลจาก researcher
  │     └── cron_add(prompt="Use http_request to fetch air4thai... if สีส้ม alert... [NO_ALERT]")
  │
  └── Confirm to user
```

**ข้อดีเทียบกับ v0.2:**
- researcher ทำ multi-step ได้ (agentic mode มี loop ของตัวเอง)
- Main agent ไม่ต้องทำ search เอง (แค่ delegate)
- cron job prompt มี tested API URL (ไม่ต้อง search ทุกรอบ)

**ข้อเสีย:**
- ช้ากว่า v0.2 (delegate + researcher loop + main agent)
- ใช้ tokens มากกว่า (2 agent loops)
- ซับซ้อนกว่า v0.2

---

## สรุป: v0.2 vs Agent Orchestration

| | v0.2 (ปัจจุบัน) | Agent Orchestration |
|---|---|---|
| Tool calls | 1 (cron_add) | 2+ (delegate + cron_add) |
| ความเร็ว | เร็ว | ช้ากว่า |
| Token cost | ต่ำ | สูงกว่า |
| ความซับซ้อน | ต่ำ | สูง |
| Cron job ต้อง search ทุกรอบ | ใช่ | ไม่ (มี tested API) |
| Gemini ทำได้ | ✅ (1 call) | ✅ (delegate = 1 call จาก main) |
| เหมาะกับ | task ง่าย-กลาง | task ซับซ้อน ต้อง research |

### แนะนำ

- **ใช้ v0.2 เป็นหลัก** สำหรับ cron jobs ทั่วไป
- **ใช้ delegate** เมื่อ task ต้อง research ก่อน (หา API, วิเคราะห์ข้อมูล)
- **ใช้ swarm** เมื่อต้องการหลายมุมมอง

---

## Next Step: Scheduler Agent (v0.3)

### ทำไมต้องมี Scheduler Agent

v0.2 ปัญหา:
- Job ต้อง google-search ทุกรอบ (Bitcoin ทุก 3 นาที = 480 search/วัน)
- Main agent ทำ multi-step ไม่ได้ (Gemini หยุดหลัง 1 call)
- AI เพิ่มเงื่อนไขเอง (context ใหญ่เกินไป)

Scheduler agent แก้ได้เพราะ:
- Context เล็ก (system prompt ~200 tokens vs main 7,500+)
- Tools น้อย (5-6 ตัว vs 80+)
- ไม่มี conversation history
- Focused prompt → Gemini มี "ที่ว่าง" ทำ multi-step

### แผน

```
User message → Main Agent → delegate(agent="scheduler") → 1 call จาก main
                                    │
                              Scheduler Agent (agentic, own loop)
                                    │
                              ├── งานง่าย → cron_add (1 call)
                              └── งานซับซ้อน → search → test → cron_add (3 calls)
```

### Config

```toml
[agents.scheduler]
provider = "custom:https://api-proxy-llm.9nerd.ai/v1"
model = "gemini-2.5-flash"
agentic = true
allowed_tools = ["shell", "http_request", "web_fetch", "cron_add", "cron_list", "cron_remove"]
max_iterations = 10
system_prompt = """You create scheduled jobs. Follow these steps:
1. If task is simple (reminder/message) → cron_add immediately
2. If task needs data → search for a reliable API, test it with http_request, then cron_add with tested URL
3. Do exactly what user asked. Don't add conditions user didn't mention.
4. Never ask for delivery channel.
5. Reply short. Reply in the same language as the user."""
```

### ต้นทุนเปรียบเทียบ (Bitcoin ทุก 3 นาที)

| | v0.2 | v0.3 (scheduler) |
|---|---|---|
| สร้าง job | 2-3s, ต่ำ | 15-30s, สูงกว่า |
| Job รัน/ครั้ง | google-search 10-20s | http_request 1-2s |
| Search calls/วัน | 480 | 0 (ใช้ tested API) |
| รวมเวลา API/วัน | ~2 ชม. | ~16 นาที |

### ความเสี่ยง

- Scheduler agent ใช้ Gemini เหมือนกัน → อาจหยุด multi-step (ต้องทดสอบ)
- สร้าง job ช้าขึ้น 15-30s
- Config ซับซ้อนขึ้น

### ขั้นตอน

1. เพิ่ม `[agents.scheduler]` ใน config.toml
2. แก้ SKILL.md ให้ delegate ไป scheduler
3. ทดสอบ "แจ้งค่าฝุ่นศรีราชา"
4. ถ้า multi-step ทำงาน → ใช้จริง
5. ถ้าไม่ → กลับ v0.2

## Status: Research — ยังไม่ implement
