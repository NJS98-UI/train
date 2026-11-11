package com.example.traework2api

import android.content.Context
import org.json.JSONObject

/**
 * 把 Trae 与 Qoder 两套账号**打包进同一个 JSON 文件**，方便换设备时"一键登录"：
 * 旧机导出一个文件 → 拷到新机 → 导入，两套账号一起回来（token 用新机自己的 KeyStore 重新加密）。
 *
 * 结构：`{"format":"traework2api-backup","version":1,"trae":{…},"qoder":{…}}`
 * 其中 `trae`/`qoder` 分别是 [TraeAuthStore.exportJson] / [QoderAuthStore.exportJson] 的整对象。
 *
 * ⚠️ 文件含**明文 token / PAT**，等价于账号钥匙，只走本地文件/剪贴板，绝不联网、绝不写日志。
 * 导入也兼容"整份文件就是单个 Trae/Qoder 导出"的老情形（按 format 字段自动识别）。
 *
 * 导入走**预览 + 确认**：先算出每个账号会「新增 / 更新 / 跳过」（Trae 有跳过保护——
 * 本地凭据更新且未过期时不许用旧备份覆盖，refresh token 是轮换的，覆盖错一次就得重登），
 * 用户勾完才真正写盘。Trae 侧预览与导入共用同一份判据（[TraeAuthStore.decideImportStatus]）。
 */
object AccountBackup {

    private fun isQoderFormat(format: String?): Boolean =
        format != null && format.contains("qoder", ignoreCase = true)

    /** @param traeUids / qoderUids null = 该平台全部；空集 = 一个都不导出。 */
    fun exportAll(
        context: Context,
        traeUids: Set<String>? = null,
        qoderUids: Set<String>? = null,
    ): String {
        val root = JSONObject()
            .put("format", "traework2api-backup")
            .put("version", 1)
            .put("exportedAt", System.currentTimeMillis() / 1000)
            .put("trae", JSONObject(TraeAuthStore.exportJson(context, traeUids)))
            .put("qoder", JSONObject(QoderAuthStore.exportJson(context, qoderUids)))
        return root.toString(2)
    }

    /** 文件里的**真实**账号条数（两平台合计）。解不开 token 的账号不会进文件，勾选数 ≠ 条数。 */
    fun countTotal(json: String): Int =
        runCatching {
            val root = JSONObject(json)
            val t = root.optJSONObject("trae")?.optJSONArray("accounts")?.length() ?: 0
            val q = root.optJSONObject("qoder")?.optJSONArray("accounts")?.length() ?: 0
            t + q
        }.getOrElse { TraeAuthStore.countAccounts(json) }

    /** 导入结果计数，供 UI 报"新增 x / 更新 y / 跳过 z"。 */
    data class ImportSummary(val trae: TraeAuthStore.ImportReport, val qoder: QoderAuthStore.QImportReport) {
        val added: Int get() = trae.added + qoder.added
        val updated: Int get() = trae.updated + qoder.updated
        val skipped: Int get() = trae.skipped
    }

    /** 导入预览：两个平台各一列。 */
    data class BackupPreview(
        val trae: List<TraeAuthStore.ImportPreview>,
        val qoder: List<QoderAuthStore.QImportPreview>,
    ) {
        val isEmpty: Boolean get() = trae.isEmpty() && qoder.isEmpty()
    }

    /**
     * 预览导入会发生什么：不加密、不写盘。
     * @throws IllegalArgumentException JSON 非法或没有可导入条目
     */
    fun previewImport(context: Context, json: String): BackupPreview {
        val root = try { JSONObject(json) } catch (_: Exception) {
            throw IllegalArgumentException("不是有效的账号备份 JSON")
        }
        val traeNode = root.optJSONObject("trae")
        val qoderNode = root.optJSONObject("qoder")
        if (traeNode != null || qoderNode != null) {
            return BackupPreview(
                trae = traeNode?.let { runCatching { TraeAuthStore.previewImport(context, it.toString()) }.getOrDefault(emptyList()) }
                    ?: emptyList(),
                qoder = qoderNode?.let { runCatching { QoderAuthStore.previewImport(context, it.toString()) }.getOrDefault(emptyList()) }
                    ?: emptyList(),
            )
        }
        // 单份导出：按 format 认出是 Trae 还是 Qoder
        return if (isQoderFormat(root.optString("format", null))) {
            BackupPreview(emptyList(), QoderAuthStore.previewImport(context, json))
        } else {
            BackupPreview(TraeAuthStore.previewImport(context, json), emptyList())
        }
    }

    /**
     * 导入账号。与 [previewImport] 的判据同源（Trae 侧共用 [TraeAuthStore.decideImportStatus]），
     * 预览说跳过的这里一定跳过。
     *
     * @throws IllegalArgumentException JSON 非法 / 没有可导入条目 / 全被跳过
     */
    fun importAll(
        context: Context,
        json: String,
        traeUids: Set<String>? = null,
        qoderUids: Set<String>? = null,
    ): ImportSummary {
        val root = try { JSONObject(json) } catch (_: Exception) {
            throw IllegalArgumentException("不是有效的账号备份 JSON")
        }
        // 备份外壳：{format:"traework2api-backup", trae:{...}, qoder:{...}}
        val traeNode = root.optJSONObject("trae")
        val qoderNode = root.optJSONObject("qoder")
        if (traeNode != null || qoderNode != null) {
            val t = traeNode?.let { runCatching { TraeAuthStore.importJson(context, it.toString(), traeUids) }.getOrNull() }
                ?: TraeAuthStore.ImportReport(0, 0, 0)
            val q = qoderNode?.let { runCatching { QoderAuthStore.importJson(context, it.toString(), qoderUids) }.getOrNull() }
                ?: QoderAuthStore.QImportReport(0, 0)
            check(t.total + q.total > 0) {
                "备份里没有可导入的账号（可能全被跳过——本地凭据更新时不许旧备份覆盖，或 token/PAT 缺失）"
            }
            return ImportSummary(t, q)
        }
        // 单份导出：按 format 认出是 Trae 还是 Qoder，整包丢给对应 store
        val format = root.optString("format", null)
        return if (isQoderFormat(format)) {
            ImportSummary(TraeAuthStore.ImportReport(0, 0, 0), QoderAuthStore.importJson(context, json, qoderUids))
        } else {
            ImportSummary(TraeAuthStore.importJson(context, json, traeUids), QoderAuthStore.QImportReport(0, 0))
        }
    }

    fun selfCheck() {
        // 格式识别：含 qoder 的走 Qoder，其余走 Trae（不能把 Trae 备份喂给 Qoder 解析）。
        check(isQoderFormat("traework2api-qoder-accounts")) { "自检失败：Qoder 导出格式应被识别" }
        check(!isQoderFormat("traework2api-accounts")) { "自检失败：Trae 导出格式不应被误判为 Qoder" }
        check(!isQoderFormat(null)) { "自检失败：null format 不应判为 Qoder" }
        // 报告聚合：跳过只来自 Trae（Qoder 的 PAT 不轮换，没有跳过规则）
        val s = ImportSummary(TraeAuthStore.ImportReport(1, 2, 3), QoderAuthStore.QImportReport(4, 5))
        check(s.added == 5 && s.updated == 7 && s.skipped == 3) { "自检失败：导入报告聚合数不对" }
    }
}
