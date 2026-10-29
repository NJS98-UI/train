// 逐行移植 TraeRegion.kt —— 区域（Region）常量集中点。
//
// 目的：把"哪些 host/端点/版本号随区域变化"收在一处，避免散落硬编码（需求 §6.3）。
// 国内版（CN）是**已实测**的；国际版（SG）目前只拿到上游 host，
// 其余字段**未经实测**，缺失的一律留空并在运行时显式报错。
//
// ⚠️ **国际服（SG）已冻结为「未开发」**（2026-09-23 用户决定，见 is_supported）：
// 登录链路是通的，但聊天不支持多轮上下文、外部工具调不了，且免费额度约 $1/月。
// **代码保留但不再投入**；UI 上明确标注并引导用户用国服。要恢复就把 is_supported 改回去。
use crate::state::AppState;
use std::sync::atomic::{AtomicU8, Ordering};

/// IDE 版本号：`User-Agent: Trae/<v>`、`X-Ide-Version` 等都用它。
/// 与区域无关（国内实测量），故放在模块级而不是每个 region 上。
pub const IDE_VERSION: &str = "0.1.43";

/// 当前生效区域的内存位（0=CN 1=SG）。默认 CN（本工程已实测的那条链路）。
/// 对齐 Kotlin `@Volatile var current: TraeRegion = CN`。
static CURRENT: AtomicU8 = AtomicU8::new(0);

const fn enc(r: TraeRegion) -> u8 {
    match r {
        TraeRegion::Cn => 0,
        TraeRegion::Sg => 1,
    }
}
const fn dec(v: u8) -> TraeRegion {
    if v == 1 {
        TraeRegion::Sg
    } else {
        TraeRegion::Cn
    }
}

/// 取当前区域（不依赖 AppState 的内部访问器；登录/转发等无 app 句柄的调用点用）。
pub fn current_global() -> TraeRegion {
    dec(CURRENT.load(Ordering::SeqCst))
}

/// 只改内存中的当前区域，**不持久化**（对齐 Kotlin `TraeRegion.current = …` 直赋值）。
pub fn set_current_global(r: TraeRegion) {
    CURRENT.store(enc(r), Ordering::SeqCst);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TraeRegion {
    Cn,
    Sg,
}

impl TraeRegion {
    /// 内部标识，用于日志与持久化。
    pub fn id(self) -> &'static str {
        match self {
            TraeRegion::Cn => "cn",
            TraeRegion::Sg => "sg",
        }
    }

    /// 聊天/模型接口 host（SOLO Agent）。
    pub fn agent_host(self) -> &'static str {
        match self {
            TraeRegion::Cn => "https://trae-api-cn.mchost.guru",
            TraeRegion::Sg => "https://core-normal.trae.ai",
        }
    }

    /// 额度/签到接口 host（User Gateway）。
    pub fn ug_host(self) -> &'static str {
        match self {
            TraeRegion::Cn => "https://api.trae.cn",
            TraeRegion::Sg => "https://ug-normal.trae.ai",
        }
    }

    /// 额度/签到请求头里的区域标识；为空则不发该头。
    pub fn user_region_header(self) -> &'static str {
        match self {
            TraeRegion::Cn => "CN",
            // 国际服未观测到该头；不发明
            TraeRegion::Sg => "",
        }
    }

    /// OAuth（ExchangeToken / GetUserInfo）host。空=该区域尚未支持。
    pub fn oauth_host(self) -> &'static str {
        match self {
            TraeRegion::Cn => "https://api.trae.com.cn",
            TraeRegion::Sg => "https://ug-normal.trae.ai",
        }
    }

    /// 登录页 host（构造登录 URL 用）。空=该区域尚未支持。
    pub fn console_host(self) -> &'static str {
        match self {
            TraeRegion::Cn => "https://www.trae.cn",
            TraeRegion::Sg => "https://www.trae.ai",
        }
    }

    /// OAuth ClientID。空=该区域尚未支持。
    pub fn client_id(self) -> &'static str {
        match self {
            // SOLO stable（实测）
            TraeRegion::Cn => "en1oxy7wnw8j9n",
            TraeRegion::Sg => "ono9krqynydwx5",
        }
    }

    /// 该区域是否已具备完整可用配置（缺 OAuth 就只能转发、不能登录）。
    pub fn can_login(self) -> bool {
        !self.oauth_host().is_empty() && !self.console_host().is_empty() && !self.client_id().is_empty()
    }

    /// 额度/签到接口是否可用。
    pub fn can_query_quota(self) -> bool {
        !self.ug_host().is_empty()
    }

    /// 该区域是否视为**已开发/受支持**。SG（国际服）**冻结为未开发**（2026-09-23 用户决定）。
    /// 判据收在这**一处**，UI/日志/守卫都读它。
    pub fn is_supported(self) -> bool {
        self == TraeRegion::Cn
    }

    /// 未开发时给用户看的一句话（已开发则为空）。
    pub fn unsupported_note(self) -> &'static str {
        if self.is_supported() {
            ""
        } else {
            "未开发：登录可通，但聊天不支持多轮上下文与外部工具调用；且免费额度极少（约 $1/月）。建议使用国服。"
        }
    }

    /// 展示名（对齐 MainActivity：`if (region == TraeRegion.SG) "国际服" else "国服"`）。
    pub fn display_name(self) -> &'static str {
        if self == TraeRegion::Sg {
            "国际服"
        } else {
            "国服"
        }
    }

    /// 按 id 查（持久化读回用）；未知/空白回退 CN。
    pub fn from_id(id: &str) -> TraeRegion {
        let t = id.trim().to_lowercase();
        match t.as_str() {
            "sg" => TraeRegion::Sg,
            _ => TraeRegion::Cn,
        }
    }

    /// 取当前区域，并断言它支持登录；不支持则给出人话错误。
    pub fn require_login(region: TraeRegion) -> Result<TraeRegion, String> {
        if !region.can_login() {
            return Err(format!(
                "区域 {} 的登录协议尚未接入（缺 OAuth host / ClientID），目前仅支持 {}",
                region.id(),
                TraeRegion::Cn.id()
            ));
        }
        Ok(region)
    }

    /// 取当前区域（内存态；对齐 Kotlin `TraeRegion.current`）。
    pub fn current(_app: &AppState) -> TraeRegion {
        current_global()
    }

    /// 切换并持久化区域。
    pub fn set_current(app: &AppState, r: TraeRegion) {
        set_current_global(r);
        app.set_pref("trae_region", r.id());
    }

    /// 从磁盘恢复区域。必须在启动时调用一次：
    /// 区域不持久化的话，重启后会回落成 CN，导致**国际服 token 被发到国内服 host**，
    /// 表现为 401/1001 或 4001（实测踩过）。
    pub fn load(app: &AppState) {
        match app.pref("trae_region") {
            Some(id) => set_current_global(TraeRegion::from_id(&id)),
            None => set_current_global(TraeRegion::Cn), // Kotlin fromId(null) → CN
        }
    }

    /// 自检区域不变式：
    /// 1) CN 必须完整可登录（已实测链路，不能被后续改动悄悄弄坏）；
    /// 2) SG 的常量必须齐全到"能发起登录"（否则 require_login 会报错，
    ///    用户点了国际服登录却什么都不发生）；
    /// 3) 两区域的 host / ClientID 必须不同——包括 consoleHost 与 clientId：
    ///    曾出过"复制登录链接"没带区域、永远生成国服链接的 bug，
    ///    所以这里把"按区域切 current 能改变生成的链接"也钉住。
    pub fn self_check() {
        let cn = TraeRegion::Cn;
        let sg = TraeRegion::Sg;
        assert!(cn.can_login() && cn.can_query_quota(), "自检失败：CN 区域应完整可用");
        assert!(cn.is_supported(), "自检失败：国服必须受支持（它是主链路）");
        assert!(!sg.is_supported(), "自检失败：国际服当前应标为未开发");
        assert!(
            cn.unsupported_note().is_empty() && !sg.unsupported_note().is_empty(),
            "自检失败：未开发说明应只在未开发区域出现"
        );
        assert!(cn.agent_host().starts_with("https://"), "自检失败：CN agentHost 应为 https");
        assert_eq!(cn.user_region_header(), "CN", "自检失败：CN 的 X-User-Region 应为 CN");
        assert!(
            sg.can_login(),
            "自检失败：SG 常量应齐全（oauthHost/consoleHost/clientId 都不能为空）"
        );
        assert!(sg.agent_host().starts_with("https://"), "自检失败：SG agentHost 应为 https");
        assert_ne!(cn.agent_host(), sg.agent_host(), "自检失败：CN/SG 的 agentHost 不应相同");
        assert_ne!(cn.oauth_host(), sg.oauth_host(), "自检失败：CN/SG 的 oauthHost 不应相同");
        assert_ne!(cn.console_host(), sg.console_host(), "自检失败：CN/SG 的 consoleHost 不应相同");
        assert_ne!(cn.client_id(), sg.client_id(), "自检失败：CN/SG 的 clientId 不应相同");
        assert!(cn.console_host().contains("trae.cn"), "自检失败：CN 登录页应在 trae.cn");
        assert!(sg.console_host().contains("trae.ai"), "自检失败：SG 登录页应在 trae.ai");
        assert!(
            TraeRegion::from_id("cn") == cn && TraeRegion::from_id("SG") == sg,
            "自检失败：fromId 应大小写无关地命中"
        );
        assert!(TraeRegion::from_id("unknown-region") == cn, "自检失败：未知区域应回退 CN");
        // Kotlin fromId(null) == CN；Rust 无 null，用空白串表达同一语义
        assert!(TraeRegion::from_id("") == cn, "自检失败：空区域应回退 CN");
    }
}
