//! Translate a resolved `ResourceClass` (from `sizing.rs`) into the
//! partition/QoS/cpus/mem/gres/time subset of an `SbatchSpec`. Kept
//! separate from `sizing.rs` so sizing stays focused on "pick the
//! right class" and this module stays focused on "turn that class into
//! SBATCH flags" — two concerns the plan separates for clarity.

use super::sbatch::SbatchSpec;
use super::sizing::ResourceClass;

/// Copy the resource-shape fields (partition, qos, cpus, mem, gres,
/// time) from `class` into `spec`. Leaves non-shape fields (job_name,
/// account, output_path, modules, exports, body) untouched so callers
/// can fill those in independently.
pub fn apply_class(spec: &mut SbatchSpec, class: &ResourceClass) {
    spec.partition = class.partition.clone();
    spec.qos = class.qos.clone();
    spec.cpus_per_task = class.cpus_per_task;
    spec.mem = class.mem.clone();
    spec.gres = class.gres.clone();
    spec.time_limit = class.time.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty_spec() -> SbatchSpec {
        SbatchSpec {
            job_name: "j".into(),
            partition: "placeholder".into(),
            qos: Some("placeholder-qos".into()),
            account: Some("acct".into()),
            cpus_per_task: 999,
            mem: "placeholder-mem".into(),
            gres: Some("placeholder-gres".into()),
            time_limit: "placeholder-time".into(),
            output_path: "out.log".into(),
            modules: vec!["m1".into()],
            exports: {
                let mut m = BTreeMap::new();
                m.insert("K".into(), "V".into());
                m
            },
            body: "body".into(),
        }
    }

    #[test]
    fn apply_class_overwrites_shape_fields() {
        let mut spec = empty_spec();
        let class = ResourceClass {
            partition: "long".into(),
            qos: Some("normal".into()),
            cpus_per_task: 16,
            mem: "64G".into(),
            gres: Some("gpu:1".into()),
            time: "12:00:00".into(),
        };
        apply_class(&mut spec, &class);
        assert_eq!(spec.partition, "long");
        assert_eq!(spec.qos.as_deref(), Some("normal"));
        assert_eq!(spec.cpus_per_task, 16);
        assert_eq!(spec.mem, "64G");
        assert_eq!(spec.gres.as_deref(), Some("gpu:1"));
        assert_eq!(spec.time_limit, "12:00:00");
    }

    #[test]
    fn apply_class_clears_qos_and_gres_when_class_omits_them() {
        let mut spec = empty_spec();
        let class = ResourceClass {
            partition: "short".into(),
            qos: None,
            cpus_per_task: 4,
            mem: "16G".into(),
            gres: None,
            time: "02:00:00".into(),
        };
        apply_class(&mut spec, &class);
        // Placeholder qos/gres must be cleared so the final sbatch
        // script doesn't leak stale state.
        assert!(spec.qos.is_none());
        assert!(spec.gres.is_none());
    }

    #[test]
    fn apply_class_leaves_non_shape_fields_alone() {
        let mut spec = empty_spec();
        let class = ResourceClass {
            partition: "p".into(),
            qos: None,
            cpus_per_task: 1,
            mem: "1G".into(),
            gres: None,
            time: "00:10:00".into(),
        };
        apply_class(&mut spec, &class);
        // Untouched:
        assert_eq!(spec.job_name, "j");
        assert_eq!(spec.account.as_deref(), Some("acct"));
        assert_eq!(spec.output_path, "out.log");
        assert_eq!(spec.modules, vec!["m1".to_string()]);
        assert_eq!(spec.body, "body");
        assert_eq!(spec.exports.get("K").map(String::as_str), Some("V"));
    }
}
