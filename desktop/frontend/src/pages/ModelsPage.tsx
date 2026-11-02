// 页签「模型」的后续波次占位（本波只做主题系统 + 原子组件 + 概览/记录/体检/外观四页复刻）。
// 用 SearchInput / SectionTitle / Card 原子搭结构，保证与其它页一致且不白屏。
import { Card, SearchInput, SectionTitle, Text, Caption } from '../components'

export function ModelsPage() {
  return (
    <>
      <SearchInput value="" placeholder="搜索模型…" onChange={() => { /* 后续波次：models_view + applyFilter */ }} />
      <SectionTitle>模型目录</SectionTitle>
      <Card tone="normal">
        <Text fs="cardTitle" bold>
          模型目录尚未加载
        </Text>
        <Caption>本波次未包含模型页复刻，后续波次按 ui-keepalive.md §2.3 补齐工具条、计数与模型卡列表。</Caption>
      </Card>
    </>
  )
}
