use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyDict};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct DataTracker {
    data: Vec<i32>,
}

impl ToPyObject for DataTracker {
    fn to_object(&self, py: Python) -> PyObject {
        let data = self
            .data
            .iter()
            .map(|d| d.to_object(py))
            .collect::<Vec<PyObject>>();
        data.to_object(py)
    }
}

fn test_fn() -> PyResult<()> {
    Python::with_gil(|py| {
        let locals = [("os", py.import_bound("os")?)].into_py_dict_bound(py);
        let code = "dt[0]";
        let dt = DataTracker {
            data: vec![12, 24, 36],
        };
        let py_dt = dt.to_object(py);

        locals.set_item("dt", py_dt)?;

        let user: i32 = py
            .eval_bound(code, Some(&locals), Some(&locals))?
            .extract()?;

        println!("Hello {}, I'm Python", user);
        Ok(())
    })
}

// tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_fn() {
        test_fn().unwrap();
        assert!(false);
    }
}
