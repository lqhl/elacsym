# Schema Behavior in Elacsym

## 概述

Elacsym 使用严格的 schema 定义来管理文档属性。只有在 schema 中声明的字段会被持久化和索引。

## Schema 中的字段 vs 额外字段

### Schema 中声明的字段

当你在 schema 中声明一个字段时:

```json
{
  "attributes": {
    "title": {
      "type": "string",
      "indexed": true,
      "full_text": {"Simple": true}
    },
    "score": {
      "type": "float",
      "indexed": false,
      "full_text": {"Simple": false}
    }
  }
}
```

**行为**:
- ✅ **持久化**: 字段会被写入 Parquet segments
- ✅ **查询返回**: 字段会在查询结果中返回
- ✅ **过滤支持**: 可以使用过滤器查询该字段
- ✅ **全文索引**: 如果 `full_text` 启用,会创建全文索引

### 额外字段 (未在 Schema 中声明)

如果你在 upsert 时包含未在 schema 中声明的字段:

```json
{
  "id": 1,
  "vector": [...],
  "attributes": {
    "title": "Declared field",
    "extra_field": "This is NOT in schema"  // ❌ 额外字段
  }
}
```

**行为**:
- ❌ **不持久化**: 字段会被**静默丢弃**,不会写入存储
- ❌ **查询不返回**: 查询结果中不会包含该字段
- ❌ **无法过滤**: 对该字段的过滤器会**返回空结果**
- ❌ **无法索引**: 不会创建任何索引

## 实验验证

### 测试 1: Upsert 包含额外字段

```bash
# Schema 只定义了 declared_field
curl -X PUT http://localhost:3000/v1/namespaces/test \
  -d '{
    "schema": {
      "attributes": {
        "declared_field": {"type": "string", "indexed": true}
      }
    }
  }'

# Upsert 包含额外字段
curl -X POST http://localhost:3000/v1/namespaces/test/upsert \
  -d '{
    "documents": [{
      "id": 1,
      "attributes": {
        "declared_field": "value1",
        "extra_field": "extra_value"  # ❌ 不在 schema 中
      }
    }]
  }'
```

**结果**: Upsert 成功,但 `extra_field` 被丢弃

### 测试 2: 查询返回的字段

```bash
curl -X POST http://localhost:3000/v1/namespaces/test/query \
  -d '{"top_k": 10}'
```

**结果**:
```json
{
  "results": [{
    "id": 1,
    "attributes": {
      "declared_field": "value1"
      // ❌ extra_field 不存在
    }
  }]
}
```

### 测试 3: 对额外字段过滤

```bash
curl -X POST http://localhost:3000/v1/namespaces/test/query \
  -d '{
    "top_k": 10,
    "filter": {
      "conditions": [
        {"field": "extra_field", "op": "eq", "value": "extra_value"}
      ]
    }
  }'
```

**结果**:
```json
{
  "results": []  // ❌ 空结果,因为字段不存在
}
```

## `indexed` 参数的作用

`indexed` 参数目前**主要用于文档化目的**,表示字段是否适合用于过滤:

```json
{
  "category": {
    "type": "string",
    "indexed": true   // 建议用于过滤
  },
  "description": {
    "type": "string",
    "indexed": false  // 不建议用于过滤
  }
}
```

### 当前实现中的行为

- `indexed: true` - 表示该字段适合用于过滤查询
- `indexed: false` - 表示该字段主要用于展示,不建议过滤

**重要**: 在当前实现中,`indexed: false` 的字段**仍然可以被过滤**,只是性能可能不如 `indexed: true`。未来版本可能会添加专门的索引优化。

## `full_text` 参数的作用

控制是否为字段创建全文搜索索引:

```json
{
  "title": {
    "type": "string",
    "full_text": {"Simple": true}  // ✅ 创建全文索引
  },
  "category": {
    "type": "string",
    "full_text": {"Simple": false}  // ❌ 不创建全文索引
  }
}
```

### 高级全文配置

```json
{
  "content": {
    "type": "string",
    "full_text": {
      "Advanced": {
        "language": "english",
        "stemming": true,
        "remove_stopwords": true,
        "case_sensitive": false
      }
    }
  }
}
```

**行为**:
- `full_text: {"Simple": true}` - 创建基础全文索引,可用于全文搜索
- `full_text: {"Simple": false}` - 不创建全文索引,**无法**用于全文搜索
- `full_text: {"Advanced": {...}}` - 创建带高级配置的全文索引

## 最佳实践

### ✅ 正确做法

1. **在 schema 中声明所有需要的字段**:
   ```json
   {
     "attributes": {
       "title": {...},
       "category": {...},
       "score": {...}  // ✅ 声明所有字段
     }
   }
   ```

2. **需要过滤的字段设置 `indexed: true`**:
   ```json
   {
     "category": {
       "type": "string",
       "indexed": true  // ✅ 用于过滤
     }
   }
   ```

3. **需要全文搜索的字段启用 `full_text`**:
   ```json
   {
     "title": {
       "type": "string",
       "full_text": {"Simple": true}  // ✅ 用于搜索
     }
   }
   ```

### ❌ 错误做法

1. **依赖额外字段**:
   ```json
   // Schema 中没有 score
   // Upsert 时包含 score
   // 尝试过滤 score  ❌ 不会工作
   ```

2. **对未启用全文索引的字段进行全文搜索**:
   ```json
   {
     "category": {
       "full_text": {"Simple": false}
     }
   }
   // 尝试全文搜索 category  ❌ 不会工作
   ```

## 原因解析

### 为什么额外字段会被丢弃?

原因在 `SegmentWriter` 的实现 (src/segment/mod.rs:36-63):

```rust
fn create_arrow_schema(schema: &Schema) -> Result<Arc<ArrowSchema>> {
    let mut fields = vec![...];

    // 只遍历 schema 中定义的字段
    for (name, attr_schema) in &schema.attributes {
        fields.push(Field::new(name, data_type, true));
    }

    Ok(Arc::new(ArrowSchema::new(fields)))
}
```

**设计意图**:
1. **类型安全**: 确保所有字段都有明确的类型定义
2. **索引一致性**: 只有声明的字段才会被索引
3. **查询优化**: 避免运行时类型推断
4. **Schema 演化**: 未来支持 schema 版本控制和迁移

## 总结

| 字段类型 | 持久化 | 查询返回 | 可过滤 | 可全文搜索 |
|---------|--------|---------|--------|-----------|
| Schema 中声明 (indexed=true, full_text=true) | ✅ | ✅ | ✅ | ✅ |
| Schema 中声明 (indexed=false, full_text=false) | ✅ | ✅ | ✅* | ❌ |
| 额外字段 (未声明) | ❌ | ❌ | ❌ | ❌ |

*虽然可以过滤,但不建议,性能可能不佳

**关键规则**: **只有在 schema 中声明的字段才会被存储和索引。额外的字段会被静默丢弃。**
