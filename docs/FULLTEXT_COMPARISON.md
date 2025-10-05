# 全文搜索 API 对比：Turbopuffer vs Elacsym

**日期**: 2025-10-05
**目的**: 调研 Turbopuffer 全文搜索设计，对比 Elacsym 当前设计，制定实现方案

---

## 📊 对比表格

| 特性 | Turbopuffer | Elacsym (当前设计) | 差距 | 优先级 |
|------|-------------|-------------------|------|--------|
| **Schema 配置** | | | | |
| 全文字段标记 | `full_text_search: true` | `full_text: bool` | ✅ 已对齐 | - |
| 高级配置 | 支持 language, stemming, stopwords | 仅 boolean 标记 | 🟡 缺少高级选项 | P2 |
| **查询语法** | | | | |
| 基础 BM25 | `["text", "BM25", "query"]` | `FullTextQuery { field, query, weight }` | ✅ 功能对齐 | - |
| 多字段搜索 | `["Sum", [...]]` 组合器 | 单字段 | 🔴 不支持 | P1 |
| 字段权重 | `["Product", [weight, [...]]]` | `weight` 字段 | ⚠️ 设计不同 | P1 |
| **搜索功能** | | | | |
| BM25 算法 | ✅ | 🔴 待实现 | 需要 Tantivy | P0 |
| 前缀查询 | `last_as_prefix` | ❌ | 🟡 缺失 | P2 |
| 短语匹配 | `ContainsAllTokens` | ❌ | 🟡 缺失 | P2 |
| **混合搜索** | | | | |
| 与向量搜索结合 | Multi-query | `QueryRequest` 同时支持 | ✅ 设计对齐 | - |
| 融合算法 | RRF (隐式) | 🔴 待实现 | 需要实现 | P1 |

---

## 🔍 Turbopuffer 全文搜索详解

### 1. Schema 配置

#### 简单配置
```json
{
  "title": {
    "type": "string",
    "full_text_search": true
  }
}
```

#### 高级配置
```json
{
  "content": {
    "type": "string",
    "full_text_search": {
      "language": "english",
      "stemming": true,           // 词干提取
      "remove_stopwords": true,   // 移除停用词
      "case_sensitive": false,    // 大小写不敏感
      "tokenizer": "word_v1",
      "k1": 1.2,                  // BM25 参数
      "b": 0.75
    }
  }
}
```

**特点**:
- 支持多种语言（stemming）
- 可自定义 BM25 参数
- 默认移除停用词

---

### 2. 查询语法

#### 单字段 BM25 查询
```json
{
  "rank_by": ["title", "BM25", "vector database"]
}
```

#### 多字段组合（Sum）
```json
{
  "rank_by": ["Sum", [
    ["title", "BM25", "whale facts"],
    ["description", "BM25", "whale facts"]
  ]]
}
```

#### 字段加权（Product）
```json
{
  "rank_by": ["Sum", [
    ["Product", [2.0, ["title", "BM25", "fox jumping"]]],  // title 权重 2x
    ["content", "BM25", "fox jumping"]                     // content 权重 1x
  ]]
}
```

#### 取最大分数（Max）
```json
{
  "rank_by": ["Max", [
    ["title", "BM25", "whale"],
    ["description", "BM25", "whale"]
  ]]
}
```

**特点**:
- 使用 S-expression 风格的嵌套语法
- 支持 `Sum`, `Max`, `Product` 组合器
- 灵活的字段权重控制

---

### 3. 混合搜索

#### 向量 + 全文
Turbopuffer 使用 **Multi-query** 实现：
```python
results = ns.query(
    multi_query=[
        {
            "rank_by": ["vector", "ANN", [0.1, 0.2, ...]],  # 向量搜索
            "weight": 0.7
        },
        {
            "rank_by": ["title", "BM25", "rust database"],  # 全文搜索
            "weight": 0.3
        }
    ],
    top_k=10
)
```

**融合方式**:
- 每个 sub-query 独立执行
- 分数按 weight 加权
- 最终按加权分数排序

---

## 🎯 Elacsym 当前设计

### 1. Schema 配置

```json
{
  "schema": {
    "attributes": {
      "title": {
        "type": "string",
        "full_text": true,    // ✅ 简单 boolean 标记
        "indexed": false
      }
    }
  }
}
```

**优点**:
- ✅ 简单直观
- ✅ 已有 `full_text` 字段

**缺点**:
- ❌ 无法配置语言、stemming 等高级选项
- ❌ 无法自定义 BM25 参数

---

### 2. 查询设计

**当前定义** (`src/query/mod.rs`):
```rust
pub struct FullTextQuery {
    pub field: String,        // 单字段
    pub query: String,        // 查询文本
    pub weight: f32,          // 权重 (默认 0.5)
}

pub struct QueryRequest {
    pub vector: Option<Vector>,
    pub full_text: Option<FullTextQuery>,   // ❌ 只支持单字段
    pub filter: Option<FilterExpression>,
    // ...
}
```

**优点**:
- ✅ 支持向量 + 全文混合查询
- ✅ 有权重字段

**缺点**:
- ❌ **只支持单字段**全文搜索
- ❌ 无法组合多个全文查询
- ❌ 没有 `Sum` / `Max` / `Product` 组合器

---

## 🚀 实现方案

### Phase 1: 基础 BM25 实现 (P0 - 本次实现)

**目标**: 最小可用的全文搜索

#### 1.1 创建 FullTextIndex 结构

**文件**: `src/index/fulltext.rs`

```rust
use tantivy::*;
use std::sync::Arc;

pub struct FullTextIndex {
    index: Index,
    reader: IndexReader,
    field_name: String,
    field: Field,
}

impl FullTextIndex {
    /// 创建全文索引
    pub fn new(field_name: String) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        // ID 字段
        let id_field = schema_builder.add_u64_field("id", INDEXED | STORED);

        // 文本字段
        let text_field = schema_builder.add_text_field(&field_name, TEXT | STORED);

        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema);
        let reader = index.reader()?;

        Ok(Self {
            index,
            reader,
            field_name,
            field: text_field,
        })
    }

    /// 添加文档到索引
    pub fn add_documents(&mut self, docs: &[(u64, String)]) -> Result<()> {
        let mut writer = self.index.writer(50_000_000)?;  // 50MB buffer

        for (id, text) in docs {
            let mut doc = Document::new();
            doc.add_u64(self.id_field, *id);
            doc.add_text(self.field, text);
            writer.add_document(doc)?;
        }

        writer.commit()?;
        Ok(())
    }

    /// BM25 搜索
    pub fn search(&self, query_text: &str, limit: usize) -> Result<Vec<(u64, f32)>> {
        let searcher = self.reader.searcher();
        let query_parser = QueryParser::for_index(&self.index, vec![self.field]);
        let query = query_parser.parse_query(query_text)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results = Vec::new();
        for (_score, doc_address) in top_docs {
            let doc = searcher.doc(doc_address)?;
            let id = doc.get_first(self.id_field).unwrap().as_u64().unwrap();
            results.push((id, _score));
        }

        Ok(results)
    }
}
```

#### 1.2 集成到 Namespace

```rust
pub struct Namespace {
    // ...
    fulltext_indexes: HashMap<String, Arc<RwLock<FullTextIndex>>>,
}

impl Namespace {
    pub async fn upsert(&self, documents: Vec<Document>) -> Result<usize> {
        // ... 写入 segment ...

        // 更新全文索引
        for (field_name, attr_schema) in &schema.attributes {
            if attr_schema.full_text {
                if let Some(index) = self.fulltext_indexes.get(field_name) {
                    let mut index = index.write().await;
                    let texts: Vec<(u64, String)> = documents
                        .iter()
                        .filter_map(|doc| {
                            doc.attributes
                                .get(field_name)
                                .and_then(|v| match v {
                                    AttributeValue::String(s) => Some((doc.id, s.clone())),
                                    _ => None,
                                })
                        })
                        .collect();

                    index.add_documents(&texts)?;
                }
            }
        }

        Ok(documents.len())
    }
}
```

#### 1.3 更新查询流程

```rust
pub async fn query(
    &self,
    query_vector: Option<&[f32]>,
    full_text_query: Option<&FullTextQuery>,
    top_k: usize,
    filter: Option<&FilterExpression>,
) -> Result<Vec<(Document, f32)>> {
    let mut vector_results = Vec::new();
    let mut fulltext_results = Vec::new();

    // 并行执行向量和全文搜索
    if let Some(vector) = query_vector {
        vector_results = self.search_vector(vector, top_k * 2).await?;
    }

    if let Some(ft_query) = full_text_query {
        if let Some(index) = self.fulltext_indexes.get(&ft_query.field) {
            let index = index.read().await;
            fulltext_results = index.search(&ft_query.query, top_k * 2)?;
        }
    }

    // 融合结果（暂时简单合并，Phase 2 实现 RRF）
    let combined = merge_results(vector_results, fulltext_results);

    // 应用过滤器...
    // 读取文档...
}
```

---

### Phase 2: 高级功能 (P1-P2)

#### 2.1 支持多字段全文搜索

**更新 FullTextQuery**:
```rust
pub struct FullTextQuery {
    pub fields: Vec<String>,    // 多字段支持
    pub query: String,
    pub weights: HashMap<String, f32>,  // 每个字段的权重
}
```

#### 2.2 实现 RRF 融合

**文件**: `src/query/fusion.rs`

```rust
pub fn reciprocal_rank_fusion(
    vector_results: &[(u64, f32)],
    fulltext_results: &[(u64, f32)],
    k: f32,  // RRF constant = 60
    alpha: f32,  // vector weight
    beta: f32,   // fulltext weight
) -> Vec<(u64, f32)> {
    let mut scores: HashMap<u64, f32> = HashMap::new();

    // Vector scores
    for (rank, (id, _)) in vector_results.iter().enumerate() {
        let rrf_score = alpha / (k + (rank as f32 + 1.0));
        *scores.entry(*id).or_insert(0.0) += rrf_score;
    }

    // Full-text scores
    for (rank, (id, _)) in fulltext_results.iter().enumerate() {
        let rrf_score = beta / (k + (rank as f32 + 1.0));
        *scores.entry(*id).or_insert(0.0) += rrf_score;
    }

    // Sort by score
    let mut results: Vec<_> = scores.into_iter().collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    results
}
```

#### 2.3 Schema 高级配置

**扩展 AttributeSchema**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullTextConfig {
    pub language: String,       // "english", "chinese", etc.
    pub stemming: bool,
    pub remove_stopwords: bool,
    pub case_sensitive: bool,
}

pub struct AttributeSchema {
    pub attr_type: AttributeType,
    pub indexed: bool,
    pub full_text: Option<FullTextConfig>,  // None = 不启用，Some = 启用
}
```

---

## 📝 决策点

### ✅ 采用 Turbopuffer 的设计
1. **BM25 算法** - 行业标准
2. **多字段搜索** - 更灵活
3. **RRF 融合** - 混合搜索必需

### ⚠️ 简化的设计
1. **组合器语法** - 暂不实现 `Sum`/`Max`/`Product`（过于复杂）
2. **高级配置** - Phase 1 只支持基础配置
3. **前缀查询** - Phase 2 实现

### ✅ Elacsym 的优势
1. **类型安全** - Rust 强类型查询结构
2. **简单 API** - 不使用 S-expression，使用 struct

---

## 🎯 本次实现范围（Phase 1）

### ✅ 实现内容
1. ✅ 创建 `FullTextIndex` 结构（Tantivy 封装）
2. ✅ 实现 `add_documents()` - 构建索引
3. ✅ 实现 `search()` - BM25 搜索
4. ✅ 集成到 `Namespace::upsert()`
5. ✅ 集成到 `Namespace::query()`
6. ⚠️ **暂不实现** RRF 融合（简单合并）
7. ⚠️ **暂不实现** 多字段搜索（单字段）

### ❌ 留待 Phase 2
1. RRF 融合算法
2. 多字段全文搜索
3. 高级 schema 配置（stemming, stopwords）
4. 前缀查询、短语匹配

---

## 📊 实现后的能力对比

| 功能 | Turbopuffer | Elacsym (Phase 1) | Elacsym (Phase 2) |
|------|-------------|-------------------|-------------------|
| BM25 搜索 | ✅ | ✅ | ✅ |
| 单字段全文 | ✅ | ✅ | ✅ |
| 多字段全文 | ✅ | ❌ | ✅ |
| 字段权重 | ✅ | ⚠️ 固定权重 | ✅ |
| RRF 融合 | ✅ | ❌ 简单合并 | ✅ |
| 高级配置 | ✅ | ❌ | ✅ |
| 前缀查询 | ✅ | ❌ | ✅ |

---

## 🚀 下一步

立即开始实现 Phase 1：
1. 创建 `src/index/fulltext.rs`
2. 实现 Tantivy 索引封装
3. 集成到 Namespace
4. 添加测试
5. 更新 API handler

**预计耗时**: 2-3 小时
