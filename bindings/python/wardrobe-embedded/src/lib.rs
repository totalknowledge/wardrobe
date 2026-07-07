use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use wardrobe_core::{Command, WardrobeEngine};

#[pyclass]
struct WardrobeEmbeddedEngine {
    target: String,
}

#[pymethods]
impl WardrobeEmbeddedEngine {
    #[staticmethod]
    fn open(target: String) -> Self {
        Self { target }
    }

    fn execute_json(&self, command_json: String) -> PyResult<String> {
        execute_command_json(self.target.clone(), command_json)
    }
}

#[pyfunction]
fn execute_command_json(target: String, command_json: String) -> PyResult<String> {
    let command: Command = serde_json::from_str(&command_json)
        .map_err(|error| PyValueError::new_err(format!("Invalid Wardrobe command JSON: {error}")))?;

    let engine = WardrobeEngine::open(&target)
        .map_err(|error| PyRuntimeError::new_err(format!("Failed to open Wardrobe engine: {error}")))?;

    let result = engine
        .execute_command(command)
        .map_err(|error| PyRuntimeError::new_err(format!("Wardrobe command failed: {error}")))?;

    serde_json::to_string(&result)
        .map_err(|error| PyRuntimeError::new_err(format!("Failed to serialize Wardrobe result: {error}")))
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<WardrobeEmbeddedEngine>()?;
    module.add_function(wrap_pyfunction!(execute_command_json, module)?)?;
    Ok(())
}
