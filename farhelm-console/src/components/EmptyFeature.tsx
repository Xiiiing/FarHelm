import type { ReactNode } from 'react'
import { Button, Card, Empty, Typography } from 'antd'

type EmptyFeatureProps = {
  title: string
  description: string
  icon: ReactNode
}

export function EmptyFeature({ title, description, icon }: EmptyFeatureProps) {
  return (
    <section aria-labelledby="feature-title" className="feature-page">
      <Typography.Title id="feature-title" level={1}>
        {title}
      </Typography.Title>
      <Card className="empty-card">
        <Empty image={icon} description={<span>{description}</span>}>
          <Button disabled>后续版本开放</Button>
        </Empty>
      </Card>
    </section>
  )
}
