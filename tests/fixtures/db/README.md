# 数据库迁移样本

`v1.db` 使用 `20261116000000_schema_v1`，包含原有 20 张表的数据及空的 `timed_policies` 表、开放运行与时钟段、零时长事实、被隐藏的网页标题、Unicode 标题和 Punycode 域名。内容均为测试数据。

`store::tests::fixture_migrates_to_identical_schema_and_keeps_boundary_data` 复制样本后执行启动流程，比较按名称排序的 `sqlite_schema`，并检查运行、64 位时间戳与完整性。每个发布 schema 增加新的样本，已发布样本保留。
