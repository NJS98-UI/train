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
 */
object AccountBackup {

    private fun isQoderFormat(format: String?): Boolean =
        format != null && format.contains("qoder", ignoreCase = true)

    fun exportAll(context: Context): String {
        val root = JSONObject()
            .put("format", "traework2api-backup")
            .put("version", 1)
            .put("trae", JSONObject(TraeAuthStore.exportJson(context)))
            .put("qoder", JSONObject(QoderAuthStore.exportJson(context)))
        return root.toString(2)
    }

    /** 导入结果计数，供 UI 报"导入了几个 Trae / 几个 Qoder"。 */
    data class ImportSummary(val trae: Int, val qoder: Int)

    fun importAll(context: Context, json: String): ImportSummary {
        val root = try { JSONObject(json) } catch (_: Exception) {
            throw IllegalArgumentException("不是有效的账号备份 JSON")
        }
        // 备份外壳：{format:"traework2api-backup", trae:{...}, qoder:{...}}
        val traeNode = root.optJSONObject("trae")
        val qoderNode = root.optJSONObject("qoder")
        if (traeNode != null || qoderNode != null) {
            val t = traeNode?.let { runCatching { TraeAuthStore.importJson(context, it.toString()) }.getOrDefault(0) } ?: 0
            val q = qoderNode?.let { runCatching { QoderAuthStore.importJson(context, it.toString()) }.getOrDefault(0) } ?: 0
            check(t + q > 0) { "备份里没有任何可导入的账号（token/PAT 可能缺失或 JSON 损坏）" }
            return ImportSummary(t, q)
        }
        // 单份导出：按 format 认出是 Trae 还是 Qoder，整包丢给对应 store
        val format = root.optString("format", null)
        return if (isQoderFormat(format)) {
            ImportSummary(0, QoderAuthStore.importJson(context, json))
        } else {
            ImportSummary(TraeAuthStore.importJson(context, json), 0)
        }
    }

    fun selfCheck() {
        // 格式识别：含 qoder 的走 Qoder，其余走 Trae（不能把 Trae 备份喂给 Qoder 解析）。
        check(isQoderFormat("traework2api-qoder-accounts")) { "自检失败：Qoder 导出格式应被识别" }
        check(!isQoderFormat("traework2api-accounts")) { "自检失败：Trae 导出格式不应被误判为 Qoder" }
        check(!isQoderFormat(null)) { "自检失败：null format 不应判为 Qoder" }
    }
}
