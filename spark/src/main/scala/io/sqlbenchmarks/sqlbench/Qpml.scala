package io.sqlbenchmarks.sqlbench

import com.fasterxml.jackson.annotation.JsonProperty
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.module.scala.DefaultScalaModule
import org.apache.spark.sql.catalyst.plans.logical.{Filter, Join, LogicalPlan, Project}
import org.apache.spark.sql.execution.datasources.{HadoopFsRelation, LogicalRelation}

case class Document(@JsonProperty("diagram") diagram: Node)

case class Node(@JsonProperty("title") title: String,
                @JsonProperty("operator") operator: String,
                @JsonProperty("inputs") inputs: java.util.List[Node])

object Qpml {

  def fromLogicalPlan(plan: LogicalPlan): String = {

    def _fromLogicalPlan(plan: LogicalPlan): Node = {
      import collection.JavaConverters._
      val children = plan.children.map(_fromLogicalPlan).asJava
      plan match {
        case f: LogicalRelation =>
          val title = f.relation.asInstanceOf[HadoopFsRelation].location.rootPaths.head.getName
          Node(title, "scan", children)
        case j: Join =>
          val title = s"${j.joinType} Join: ${j.condition}"
          Node(title, "join", children)
        case p: Project =>
          val title = s"Projection: ${p.projectList.mkString(", ")}"
          Node(title, "projection", children)
        case f: Filter =>
          val title = s"Filter: ${f.condition}"
          Node(title, "filter", children)
        case _ =>
          val title = plan.simpleStringWithNodeId()
          Node(title, plan.getClass.getSimpleName, children)
      }
    }

    val mapper = new ObjectMapper(new YAMLFactory())
    mapper.registerModule(DefaultScalaModule)

    val query = _fromLogicalPlan(plan)
    val doc = Document(query)
    mapper.writeValueAsString(doc)
  }

}
