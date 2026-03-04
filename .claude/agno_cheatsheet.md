# Agno Framework Cheat Sheet
> Source: https://github.com/agno-agi/agno | Docs: https://docs.agno.com

## Install Agno Claude Code Skill (official, never need to look this up again)
```bash
/plugin marketplace add agno-agi/agno-skills   # inside Claude Code
/plugin install agno@agno-skills
# After: use /agno <question> directly
```

## Install Package
```bash
uv add agno
# with extras:
uv add "agno[os]" anthropic mcp
```

---

## 1 — AGENT (atomic unit)

```python
from agno.agent import Agent
from agno.models.openai_like import OpenAILike   # ← our LM Studio / Ollama
from agno.storage.sqlite import SqliteStorage

agent = Agent(
    name="My Agent",
    id="my-agent",

    # Model — use OpenAILike for our local LLM server
    model=OpenAILike(
        id="model-name",
        base_url="http://100.74.210.70:1234/v1",
        api_key="not-needed",
    ),

    # Instructions
    description="What this agent does",
    instructions=["Rule 1", "Rule 2"],   # or single string
    system_message="Full override",       # replaces auto system msg

    # Tools
    tools=[MyToolkit()],
    tool_call_limit=10,

    # Structured output
    output_schema=MyPydanticModel,

    # Session & storage
    storage=SqliteStorage(table_name="agents", db_file="data/agents.db"),
    session_id="session-123",
    user_id="user@example.com",
    add_history_to_context=True,
    num_history_runs=5,

    # Memory (choose one)
    enable_agentic_memory=True,      # agent decides when to save (efficient)
    update_memory_on_run=False,      # extract after every run (costly)

    # Knowledge (RAG)
    knowledge=knowledge_base,
    add_knowledge_to_context=True,

    # Reasoning
    reasoning=True,
    reasoning_min_steps=1,
    reasoning_max_steps=10,

    # Reliability
    retries=3,
    exponential_backoff=True,

    # Debug
    debug_mode=False,
    markdown=True,
)
```

### Running
```python
# Sync
response = agent.run("message")
print(response.content)

# Async
response = await agent.arun("message")

# Streaming
for chunk in agent.run("message", stream=True):
    print(chunk, end="")

# Pretty print
agent.print_response("message", stream=True, markdown=True)
await agent.aprint_response("message", stream=True)

# Multimodal
from agno.media import Image, Audio, Video, File
agent.run("Describe", images=[Image(url="https://...")])
agent.run("Transcribe", audio=[Audio(filepath="audio.mp3")])
agent.run("Read", files=[File(filepath="doc.pdf")])
```

### RunOutput
```python
response.content      # str or Pydantic model
response.messages     # full message list
response.metrics      # token counts, timing
response.run_id
response.session_id
```

---

## 2 — TEAM (multi-agent)

```python
from agno.team.team import Team

team = Team(
    members=[agent1, agent2],          # Agent or Team instances
    name="My Team",
    model=OpenAILike(...),             # leader model
    role="Leader role description",

    # Execution mode — pick one:
    mode="coordinate",    # supervisor: leader picks members, synthesizes (DEFAULT)
    mode="route",         # router: pick ONE member per query, return directly
    mode="broadcast",     # fan-out: ALL members run, leader synthesizes
    mode="tasks",         # autonomous: leader decomposes into task list

    max_iterations=10,
    respond_directly=False,
    show_members_responses=True,
    add_team_history_to_members=False,

    storage=SqliteStorage(...),
    session_id="team-session",
    user_id="user@example.com",
    add_history_to_context=True,
)
```

### Team Modes Summary
| Mode | Pattern | When to use |
|------|---------|-------------|
| `coordinate` | Supervisor picks & synthesizes | General multi-agent |
| `route` | Specialist dispatch (1 agent) | Fast routing to experts |
| `broadcast` | All agents run in parallel | Multiple perspectives |
| `tasks` | Leader creates task list, tracks | Autonomous complex work |

### Nested Teams
```python
research_team = Team(members=[web_agent, arxiv_agent], mode="broadcast")
analysis_team = Team(members=[data_agent, viz_agent], mode="coordinate")
main_team = Team(members=[research_team, analysis_team], mode="coordinate")
```

---

## 3 — WORKFLOW (deterministic pipeline)

```python
from agno.workflow import Workflow, Step, Steps, Parallel, Condition, Loop, Router

workflow = Workflow(
    name="My Pipeline",
    steps=[step1, step2, step3],
    storage=SqliteStorage(...),
    session_id="wf-session",
    debug_mode=False,
)
```

### Step Types
```python
# Single step — agent OR team OR custom executor
Step(name="Gather", agent=my_agent, max_retries=3, skip_on_failure=False)
Step(name="Team step", team=my_team)
Step(name="Custom", executor=my_fn)   # fn(StepInput) -> StepOutput

# Sequential pipeline
Steps(steps=[step1, step2, step3])

# Parallel (concurrent)
Parallel(step_a, step_b, step_c, name="Parallel")

# Conditional
Condition(
    evaluator=lambda input: "urgent" in input.input.lower(),
    steps=urgent_step,
    else_steps=normal_step,
)
# CEL string: evaluator='input.contains("urgent")'
# CEL vars: input, previous_step_content, previous_step_outputs, session_state

# Loop until condition
Loop(
    steps=[review_step, refine_step],
    max_iterations=5,
    end_condition=lambda outputs: "APPROVED" in outputs[-1].content,
)
# CEL vars: current_iteration, last_step_content, step_outputs

# Dynamic routing
Router(
    selector=lambda input: simple_step if len(input.input) < 100 else complex_step,
    choices=[simple_step, complex_step],
)
```

### Running Workflows
```python
response = workflow.run("Input")
workflow.print_response("Input", stream=True)
response = await workflow.arun("Input")
```

---

## 4 — MODELS / PROVIDERS

```python
# String shorthand
agent = Agent(model="openai:gpt-4o")
agent = Agent(model="anthropic:claude-sonnet-4-6")

# Full class (more control)
from agno.models.anthropic import Claude
from agno.models.openai import OpenAIChat
from agno.models.openai_like import OpenAILike   # ← OUR LOCAL SERVER
from agno.models.ollama import Ollama

# OpenAILike = any OpenAI-compatible endpoint
model = OpenAILike(
    id="lmstudio-community/gemma-3-4b",
    base_url="http://100.74.210.70:1234/v1",
    api_key="lm-studio",
)

# Common params on all models
model = Claude(
    id="claude-sonnet-4-6",
    temperature=0.7,
    max_tokens=2048,
    top_p=0.9,
)
```

### Provider Tiers
| Tier | Examples |
|------|---------|
| Cloud | OpenAI, Anthropic, Google, AWS, Azure |
| Inference | Groq, Mistral, Cohere, Together |
| Local | Ollama, LM Studio, llama.cpp, VLLM |
| Proxy | LiteLLM, `OpenAILike` (any /v1 endpoint) |

---

## 5 — TOOLS

```python
from agno.tools import tool, Toolkit

# Simple function tool
@tool
def get_weather(city: str) -> str:
    """Get current weather for a city."""
    return f"72°F, sunny in {city}"

# With options
@tool(show_result=True, requires_confirmation=True, cache_results=True)
def dangerous_action(param: str) -> str: ...

# Async tool
@tool
async def fetch_data(url: str) -> str:
    async with httpx.AsyncClient() as c:
        return (await c.get(url)).text

# Toolkit class (group related tools)
class MyToolkit(Toolkit):
    def __init__(self):
        super().__init__(name="my_toolkit")
        self.register(self.tool_one)
        self.register(self.tool_two)
    def tool_one(self, x: str) -> str: ...
    def tool_two(self, x: str) -> str: ...
```

### 120+ Built-in Tools (key ones)
| Category | Tools |
|---------|-------|
| Web Search | DuckDuckGo, Tavily, Brave, Exa |
| Data | DuckDB, PostgreSQL, Pandas, CSV |
| Content | Wikipedia, Arxiv, PubMed, HackerNews |
| APIs | GitHub, Jira, Slack, Gmail, Notion, Discord |
| AI/Media | DALL-E, ElevenLabs, Replicate |
| Finance | YFinance, OpenBB |
| System | Shell, file ops, Python exec |
| Protocol | MCP (MCPTools, MultiMCPTools) |

---

## 6 — MCP INTEGRATION

```python
from agno.tools.mcp import MCPTools, MultiMCPTools

# stdio — local CLI tool
async with MCPTools("uvx mcp-server-git") as tools:
    agent = Agent(tools=[tools])
    await agent.arun("List recent commits")

# HTTP — remote MCP server
async with MCPTools(url="http://server/mcp") as tools: ...

# Multiple servers simultaneously
async with MultiMCPTools(
    commands=["uvx mcp-server-git", "npx @modelcontextprotocol/server-filesystem ."],
    urls=["http://server2/mcp"],
) as tools:
    agent = Agent(tools=[tools])
```

Note: All MCP operations are async-only.

---

## 7 — STORAGE & MEMORY

```python
# Storage (session history)
from agno.storage.sqlite import SqliteStorage
from agno.storage.postgres import PostgresStorage

storage = SqliteStorage(table_name="agents", db_file="data/agents.db")  # dev
storage = PostgresStorage(table_name="agents", db_url="postgresql://...")  # prod

# Memory (user facts across sessions)
from agno.memory.v2.db.sqlite import SqliteMemoryDb
from agno.memory.v2.memory import Memory

memory_db = SqliteMemoryDb(db_file="data/memory.db")
memory = Memory(db=memory_db)

agent = Agent(
    storage=storage,
    memory=memory,
    enable_agentic_memory=True,   # agent decides when to save
)
```

---

## 8 — LEARNING MACHINE

```python
from agno.learning import LearningMachine, LearningMode
from agno.learning.user_profile import UserProfileConfig
from agno.learning.user_memory import UserMemoryConfig

agent = Agent(
    learning=True,   # all stores with defaults
    # OR
    learning=LearningMachine(
        user_profile=UserProfileConfig(mode=LearningMode.ALWAYS),
        user_memory=UserMemoryConfig(mode=LearningMode.AGENTIC),
    ),
)
```

| Store | What it holds |
|-------|-------------|
| User Profile | Structured: name, preferences |
| User Memory | Unstructured observations |
| Session Context | Current session state |
| Entity Memory | Facts about external entities |
| Learned Knowledge | Cross-user patterns |

| Mode | Behaviour |
|------|---------|
| ALWAYS | Auto-extract after every response |
| AGENTIC | Agent decides via tool calls (efficient) |
| PROPOSE | Agent proposes, human confirms |

---

## 9 — OPENAGENT ROADMAP ↔ AGNO MAPPING

| OpenAgent roadmap item | Agno component to use |
|------------------------|----------------------|
| Provider layer (LLM client) | `OpenAILike(base_url=..., id=...)` replaces our httpx providers |
| Agent loop (nanobot pattern) | `Agent(reasoning=True, tool_call_limit=40)` |
| Multi-agent orchestration | `Team(mode="coordinate")` or `Team(mode="tasks")` |
| Session / memory (SQLite) | `SqliteStorage` + `Memory(db=SqliteMemoryDb(...))` |
| Routing (fast vs strong model) | `Team(mode="route", members=[fast_agent, strong_agent])` |
| Tool calls / MCP-lite | `@tool` decorator or `MCPTools` for external |
| Deterministic pipelines | `Workflow(steps=[Step, Parallel, Condition])` |
| Streaming responses | `agent.arun(..., stream=True)` → async generator |
| WhatsApp / Discord channels | Stay Python; pass messages into `agent.arun()` |
| Go services (compute tools) | Expose via MCP stdio server → `MCPTools("./bin/service")` |

### Key architectural insight
Agno's `Agent` + `Team` + `Workflow` replaces our planned:
- openagent/providers/ (keep as thin wrapper or use OpenAILike directly)
- Agent loop (use Agent with tool_call_limit + reasoning)
- ServiceManager orchestration (use Team)
- Worker/message bus (use Workflow + async)

Our MCP-lite Go services can be wrapped as MCP stdio servers (Go has MCP SDK support) so `MCPTools("./bin/hello-service")` works out of the box.

---

## 10 — PRODUCTION PATTERN

```python
from agno.app.fastapi.app import FastAPIApp

# AgentOS — wraps agents as FastAPI services
fast_app = FastAPIApp(agents=[my_agent])
app = fast_app.get_app()
# uvicorn main:app
```

Or integrate directly into our existing FastAPI app:
```python
@app.post("/chat")
async def chat(msg: str, session_id: str):
    result = await agent.arun(msg, session_id=session_id, stream=False)
    return {"reply": result.content}

@app.websocket("/ws/chat")
async def ws_chat(ws: WebSocket):
    await ws.accept()
    msg = await ws.receive_text()
    async for chunk in await agent.arun(msg, stream=True):
        await ws.send_text(chunk)
```
