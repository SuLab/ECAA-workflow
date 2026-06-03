# Schema-authoring burden

Measured offline from the committed tree. The closed tool/atom/
modality vocabulary is the schema a contributor extends.

| metric | value |
| --- | --- |
| atom YAMLs | 93 |
| modality manifests | 22 |
| modality total LOC | 774 |
| archetypes | 31 |
| archetype total LOC | 1811 |
| Tool::COUNT (parsed) | 22 |
| BlockerKind variants | 47 |
| files to add a modality | 3 |

Median added LOC per new-modality commit: 871 (over 1 commits).

Files to add a modality:
- `config/modalities/<id>.yaml`
- `config/archetypes/<id>.yaml`
- `crates/core (classifier or composer test case)`
